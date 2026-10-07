import contextlib
import http.server
import json
import os
import queue
import signal
import sqlite3
import subprocess
import threading
import time
from pathlib import Path


def wait_for(description, predicate, timeout=15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.05)
    raise AssertionError(f"Timed out waiting for {description}")


class Reply:
    def __init__(self, text=None, script=None, held=False):
        self.text = text
        self.script = script
        self.release = threading.Event()
        if not held:
            self.release.set()
        self.started = threading.Event()


class Provider:
    def __init__(self):
        self.replies = queue.Queue()
        self.all_replies = []
        self.requests = []
        self.errors = []
        provider = self

        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *_args):
                pass

            def do_GET(self):
                body = json.dumps({"data": [{"id": "mock-model"}]}).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def do_POST(self):
                size = int(self.headers["Content-Length"])
                provider.requests.append(json.loads(self.rfile.read(size)))
                try:
                    reply = provider.replies.get(timeout=5)
                except queue.Empty:
                    provider.errors.append("Unexpected model request")
                    self.send_error(500)
                    return
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Connection", "close")
                self.end_headers()
                self.close_connection = True

                def emit(delta, finish=None):
                    chunk = {
                        "choices": [
                            {"index": 0, "delta": delta, "finish_reason": finish}
                        ]
                    }
                    self.wfile.write(f"data: {json.dumps(chunk)}\n\n".encode())
                    self.wfile.flush()

                try:
                    if reply.script is not None:
                        emit(
                            {
                                "tool_calls": [
                                    {
                                        "index": 0,
                                        "id": "call-script",
                                        "type": "function",
                                        "function": {
                                            "name": "kraai_nushell",
                                            "arguments": json.dumps(
                                                {"input": reply.script}
                                            ),
                                        },
                                    }
                                ]
                            },
                            "tool_calls",
                        )
                    else:
                        emit({"content": reply.text})
                    reply.started.set()
                    if not reply.release.wait(timeout=180):
                        provider.errors.append("Held model reply timed out")
                        return
                    if reply.script is None:
                        emit({}, "stop")
                    self.wfile.write(b"data: [DONE]\n\n")
                    self.wfile.flush()
                except (BrokenPipeError, ConnectionResetError):
                    pass

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def add(self, **kwargs):
        reply = Reply(**kwargs)
        self.all_replies.append(reply)
        self.replies.put(reply)
        return reply

    def close(self):
        for reply in self.all_replies:
            reply.release.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)


class Client:
    def __init__(self, binary, root, name):
        self.root = root
        self.name = name
        self.log_path = root / f"{name}.log"
        self.log = self.log_path.open("w")
        self.process = subprocess.Popen(
            [
                binary,
                "--provider",
                "mock",
                "--model",
                "mock-model",
                "--agent-profile",
                "workspace-test",
                "--storage-root",
                str(root / "state"),
                "--provider-config",
                str(root / "providers.toml"),
            ],
            cwd=root,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=self.log,
            text=True,
            start_new_session=True,
        )
        self.messages = queue.Queue()
        self.descendants = []
        self.next_id = 0
        self.reader = threading.Thread(target=self.read, daemon=True)
        self.reader.start()
        try:
            response = self.request(
                "initialize", {"protocolVersion": 2, "clientCapabilities": {}}
            )
            assert "result" in response, response
        except BaseException:
            self.close()
            raise

    def read(self):
        try:
            for line in self.process.stdout:
                self.messages.put(json.loads(line))
        except (OSError, json.JSONDecodeError) as error:
            self.messages.put(error)
        finally:
            self.messages.put(None)

    def send(self, value):
        self.process.stdin.write(json.dumps(value) + "\n")
        self.process.stdin.flush()

    def start(self, method, params):
        self.next_id += 1
        self.send(
            {"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params}
        )
        return self.next_id

    def until(self, predicate, timeout=20):
        deadline = time.monotonic() + timeout
        while True:
            try:
                value = self.messages.get(timeout=max(0, deadline - time.monotonic()))
            except queue.Empty:
                raise AssertionError(
                    f"{self.name}: timed out waiting for ACP output"
                ) from None
            assert isinstance(value, dict), f"{self.name}: ACP output closed: {value}"
            if predicate(value):
                return value

    def response(self, request_id, timeout=20):
        return self.until(
            lambda value: value.get("id") == request_id and "method" not in value,
            timeout,
        )

    def request(self, method, params):
        return self.response(self.start(method, params))

    def session(self):
        response = self.request(
            "session/new", {"cwd": str(self.root), "mcpServers": []}
        )
        assert "result" in response, response
        return response["result"]["sessionId"]

    def load(self, session):
        response = self.request(
            "session/load",
            {"sessionId": session, "cwd": str(self.root), "mcpServers": []},
        )
        assert "result" in response, response

    def prompt(self, session, text):
        return self.start(
            "session/prompt",
            {"sessionId": session, "prompt": [{"type": "text", "text": text}]},
        )

    def approve(self):
        request = self.until(
            lambda value: value.get("method") == "session/request_permission"
        )
        self.send(
            {
                "jsonrpc": "2.0",
                "id": request["id"],
                "result": {"outcome": {"outcome": "selected", "optionId": "allow"}},
            }
        )

    def track_descendants(self):
        pending = [self.process.pid]
        seen = set(pending)
        while pending:
            for path in Path(f"/proc/{pending.pop()}/task").glob("*/children"):
                with contextlib.suppress(FileNotFoundError, ProcessLookupError):
                    for child in map(int, path.read_text().split()):
                        if child not in seen:
                            seen.add(child)
                            self.descendants.append(os.pidfd_open(child))
                            pending.append(child)

    def close(self):
        if self.process.poll() is None:
            self.track_descendants()
            self.process.send_signal(signal.SIGCONT)
            self.process.stdin.close()
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
        with contextlib.suppress(ProcessLookupError):
            os.killpg(self.process.pid, signal.SIGKILL)
        for descriptor in self.descendants:
            with contextlib.suppress(ProcessLookupError):
                signal.pidfd_send_signal(descriptor, signal.SIGKILL)
            os.close(descriptor)
        self.reader.join(timeout=2)
        self.process.stdout.close()
        self.log.close()


class Fixture:
    def __init__(self, binary, root):
        self.binary = binary
        self.root = root
        root.mkdir()
        (root / ".kraai").mkdir()
        (root / ".kraai/agents.toml").write_text("""[[profiles]]
id = "workspace-test"
display_name = "Workspace Test"
description = "VM test profile"
system_prompt = "VM test"
commands = []
capabilities = ["workspace-read"]
escalation_policy = "prompt"
environment = "minimal"
nushell_startup = "clean"
path = "inherit"
""")
        self.provider = Provider()
        port = self.provider.server.server_port
        (root / "providers.toml").write_text(f"""[[provider]]
id = "mock"
type = "openai-chat-completions"
base_url = "http://127.0.0.1:{port}/v1"
api_key = "test-key"
only_listed_models = true

[[model]]
id = "mock-model"
provider_id = "mock"
supports_images = true
""")
        self.clients = []
        self.database = root / "state/data/kraai.sqlite3"

    def client(self, name):
        client = Client(self.binary, self.root, name)
        self.clients.append(client)
        return client

    def query(self, sql, params=()):
        with contextlib.closing(
            sqlite3.connect(self.database, timeout=5)
        ) as connection:
            return connection.execute(sql, params).fetchall()

    def lease(self, session):
        rows = self.query(
            "SELECT lease_active, lease_expires_at FROM sessions WHERE id = ?",
            (session,),
        )
        assert len(rows) == 1, rows
        return rows[0]

    def idle(self, session):
        wait_for("lease release", lambda: not self.lease(session)[0])

    def records(self, session, kind="message"):
        return {
            record_id: json.loads(data)
            for record_id, data in self.query(
                "SELECT id, data FROM records WHERE session_id = ? AND kind = ?",
                (session, kind),
            )
        }

    def close(self):
        for client in self.clients:
            client.close()
        self.provider.close()

    def logs(self):
        for path in self.root.glob("*.log"):
            print(f"{path}:\n{path.read_text()[-12000:]}", flush=True)
