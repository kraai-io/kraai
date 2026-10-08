import contextlib
import json
import os
import signal
import sqlite3
import sys
import tempfile
import time
from pathlib import Path

from shared_sessions_support import Fixture, wait_for


def completed(client, request):
    response = client.response(request)
    assert response.get("result", {}).get("stopReason") == "end_turn", response


def rejected(fixture, client, session, marker):
    before = fixture.records(session)
    calls = len(fixture.provider.requests)
    response = client.response(client.prompt(session, marker))
    assert "error" in response, response
    assert "another instance" in json.dumps(response), response
    after = fixture.records(session)
    assert marker not in json.dumps(after), after
    assert before.keys() == after.keys(), (before, after)
    assert len(fixture.provider.requests) == calls, fixture.provider.requests


def started(reply):
    assert reply.started.wait(timeout=15), "Provider did not receive the prompt"


def running(fixture, session):
    def find_running():
        return next(
            (
                record
                for record in fixture.records(session, "execution").values()
                if record["phase"] == "running"
            ),
            None,
        )

    return wait_for("script Running record", find_running)


def script_output(fixture, execution_id):
    return b"".join(
        row[0]
        for row in fixture.query(
            "SELECT bytes FROM execution_output WHERE execution_id = ? ORDER BY sequence",
            (execution_id,),
        )
    )


def integrity(fixture):
    assert fixture.query("PRAGMA integrity_check") == [("ok",)]
    assert fixture.query("PRAGMA foreign_key_check") == []
    messages = {
        record_id: json.loads(data)
        for record_id, data in fixture.query(
            "SELECT id, data FROM records WHERE kind = 'message'"
        )
    }
    for message in messages.values():
        parent = message["parent_id"]
        assert parent is None or parent in messages, message
        assert not isinstance(message["status"], dict), message
    for session, tip, active in fixture.query(
        "SELECT id, tip_id, lease_active FROM sessions"
    ):
        assert not active, session
        seen = set()
        while tip is not None:
            assert tip in messages and tip not in seen, (session, tip)
            seen.add(tip)
            tip = messages[tip]["parent_id"]
    assert not fixture.provider.errors, fixture.provider.errors
    assert fixture.provider.replies.empty(), "Expected model replies were not used"


def concurrent_clients(fixture):
    first = fixture.client("first")
    second = fixture.client("second")
    shared = first.session()
    second.load(shared)
    assert fixture.lease(shared)[0] == 0, "Loading a session claimed it"
    other = second.session()
    first_reply = fixture.provider.add(text="first-process-partial", held=True)
    first_prompt = first.prompt(shared, "first-process-input")
    started(first_reply)
    wait_for(
        "stream persisted",
        lambda: "first-process-partial" in json.dumps(fixture.records(shared)),
    )
    rejected(fixture, second, shared, "rejected-shared-input")
    second_reply = fixture.provider.add(text="second-process-partial", held=True)
    second_prompt = second.prompt(other, "second-process-input")
    started(second_reply)
    assert fixture.lease(shared)[0] and fixture.lease(other)[0]
    second_reply.release.set()
    completed(second, second_prompt)
    fixture.idle(other)
    assert fixture.lease(shared)[0], (
        "Finishing another session released the shared lease"
    )
    first_reply.release.set()
    completed(first, first_prompt)
    fixture.idle(shared)
    fixture.provider.add(text="second-owner-finished")
    completed(second, second.prompt(shared, "second-owner-input"))
    fixture.idle(shared)
    history = json.dumps(fixture.records(shared))
    assert "first-process-input" in history and "second-owner-input" in history, history
    integrity(fixture)


def script_heartbeat(fixture):
    first = fixture.client("owner")
    second = fixture.client("observer")
    session = first.session()
    second.load(session)
    release_file = fixture.root / "heartbeat-release"
    fixture.provider.add(
        script="# timeout=90sec permissions=no-sandbox\nprint 'heartbeat-started'; "
        f"while not ({json.dumps(str(release_file))} | path exists) {{ sleep 100ms }}; "
        "print 'heartbeat-script'"
    )
    fixture.provider.add(text="script-finished")
    prompt = first.prompt(session, "long-script-input")
    first.approve()
    execution = running(fixture, session)
    wait_for(
        "script started executing",
        lambda: b"heartbeat-started" in script_output(fixture, execution["id"]),
    )
    initial_expiry = fixture.lease(session)[1]
    print("  Script running; waiting beyond its initial 30-second lease", flush=True)
    wait_for(
        "initial lease deadline", lambda: time.time_ns() > initial_expiry, timeout=40
    )
    current_active, current_expiry = fixture.lease(session)
    assert (
        current_active
        and current_expiry > initial_expiry
        and current_expiry > time.time_ns()
    )
    assert fixture.records(session, "execution")[execution["id"]]["phase"] == "running"
    rejected(fixture, second, session, "rejected-during-script")
    release_file.touch()
    completed(first, prompt)
    fixture.idle(session)
    record = fixture.records(session, "execution")[execution["id"]]
    assert record["phase"] == "finished" and record["status"] == "completed", record
    output = script_output(fixture, execution["id"])
    assert b"heartbeat-script" in output, output
    integrity(fixture)


def killed_owner(fixture):
    first = fixture.client("killed-owner")
    second = fixture.client("replacement")
    session = first.session()
    second.load(session)
    fixture.provider.add(
        script="# timeout=120sec permissions=no-sandbox\nprint 'orphan-started'; sleep 90sec; print 'orphan-script'"
    )
    first.prompt(session, "crashed-script-input")
    first.approve()
    execution = running(fixture, session)
    wait_for(
        "script started executing",
        lambda: b"orphan-started" in script_output(fixture, execution["id"]),
    )
    first.track_descendants()
    first.process.kill()
    first.process.wait(timeout=5)
    active, expiry = fixture.lease(session)
    assert active and expiry > time.time_ns()
    rejected(fixture, second, session, "rejected-before-crash-expiry")
    print("  Owner killed; waiting for its lease to expire", flush=True)
    wait_for("crashed owner expiry", lambda: time.time_ns() > expiry, timeout=40)
    assert fixture.records(session, "execution")[execution["id"]]["phase"] == "running"
    fixture.provider.add(text="crash-recovered")
    completed(second, second.prompt(session, "crash-takeover-input"))
    fixture.idle(session)
    record = fixture.records(session, "execution")[execution["id"]]
    assert record["phase"] == "finished" and record["status"] == "runtime-error", record
    assert record["result_message_id"] in fixture.records(session), record
    fixture.provider.add(text="recovery-remains-usable")
    completed(second, second.prompt(session, "after-crash-recovery-input"))
    fixture.idle(session)
    recovered = fixture.records(session, "execution")
    assert len(recovered) == 1 and recovered[execution["id"]] == record, recovered
    integrity(fixture)


def paused_owner(fixture):
    first = fixture.client("paused-owner")
    second = fixture.client("new-owner")
    session = first.session()
    second.load(session)
    old_reply = fixture.provider.add(text="old-owner-partial", held=True)
    old_prompt = first.prompt(session, "paused-owner-input")
    started(old_reply)
    old_id = wait_for(
        "old streaming placeholder",
        lambda: next(
            (
                record_id
                for record_id, record in fixture.records(session).items()
                if "old-owner-partial" in json.dumps(record)
            ),
            None,
        ),
    )
    with contextlib.closing(sqlite3.connect(fixture.database, timeout=5)) as connection:
        connection.execute("BEGIN IMMEDIATE")
        first.process.send_signal(signal.SIGSTOP)
        wait_for(
            "owner stopped",
            lambda: (
                "\nState:\tT" in Path(f"/proc/{first.process.pid}/status").read_text()
            ),
        )
        connection.rollback()
    active, expiry = fixture.lease(session)
    assert active
    print("  Owner paused; waiting for its lease to expire", flush=True)
    wait_for("paused owner expiry", lambda: time.time_ns() > expiry, timeout=40)
    new_reply = fixture.provider.add(text="new-owner-partial", held=True)
    new_prompt = second.prompt(session, "paused-owner-takeover-input")
    started(new_reply)
    wait_for(
        "new owner placeholder",
        lambda: "new-owner-partial" in json.dumps(fixture.records(session)),
    )
    assert old_id not in fixture.records(session)
    first.process.send_signal(signal.SIGCONT)
    old_reply.release.set()
    response = first.response(old_prompt)
    assert (
        "error" in response
        or response.get("result", {}).get("stopReason") == "cancelled"
    ), response
    assert fixture.lease(session)[0], "Resumed owner released the replacement's lease"
    rejected(fixture, first, session, "rejected-resumed-owner-input")
    assert old_id not in fixture.records(session)
    new_reply.release.set()
    completed(second, new_prompt)
    fixture.idle(session)
    fixture.provider.add(text="resumed-owner-next-turn")
    completed(first, first.prompt(session, "resumed-owner-next-input"))
    fixture.idle(session)
    assert old_id not in fixture.records(session), (
        "Old owner resurrected its placeholder"
    )
    assert "new-owner-partial" in json.dumps(fixture.records(session))
    integrity(fixture)


def main():
    binary = os.path.abspath(sys.argv[1])
    root = Path(tempfile.mkdtemp(prefix="kraai-sessions-"))
    print(f"Testing {binary}; logs in {root}", flush=True)
    for test in (concurrent_clients, script_heartbeat, killed_owner, paused_owner):
        started_at = time.monotonic()
        print(f"Running {test.__name__}", flush=True)
        fixture = Fixture(binary, root / test.__name__)
        try:
            test(fixture)
        except BaseException:
            fixture.logs()
            raise
        finally:
            fixture.close()
        print(
            f"Passed {test.__name__} in {time.monotonic() - started_at:.1f}s",
            flush=True,
        )
    print("All shared-session process tests passed", flush=True)


if __name__ == "__main__":
    main()
