import os
from pathlib import Path
import shlex
import shutil
import stat
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import AsyncMock, patch

from harbor.environments.docker.docker_unix import UnixOps
from kraai_harbor.cli import preserve_download_permissions


@unittest.skipUnless(os.name == "posix", "POSIX directory permissions")
class DownloadPermissionsTests(unittest.IsolatedAsyncioTestCase):
    async def test_download_preserves_source_modes_under_restrictive_umask(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source with 'quotes'"
            source.mkdir()
            (source / "run").write_text("exit 0\n")
            (source / "run").chmod(0o751)
            (source / "private").mkdir(mode=0o700)
            (source / "private/data").write_text("private")
            (source / "private/data").chmod(0o600)

            async def copy_contents(args, check):
                self.assertTrue(check)
                self.assertEqual(args[0], "cp")
                shutil.copytree(source, target, dirs_exist_ok=True)
                target.chmod(0o700)

            async def directory_mode(command, **kwargs):
                self.assertEqual(
                    shlex.split(command), ["stat", "-L", "-c", "%a", "--", str(source)]
                )
                self.assertEqual(
                    kwargs, {"service": service, "user": "root", "timeout_sec": 10}
                )
                return SimpleNamespace(return_code=0, stdout=f"{mode:o}\n", stderr="")

            env = SimpleNamespace(
                service_exec=directory_mode,
                _run_docker_compose_command=copy_contents,
            )
            original = UnixOps.download_dir
            with preserve_download_permissions():
                for mode, service in [
                    (0o755, None), (0o750, "sidecar"), (0o700, None), (0o1777, None)
                ]:
                    with self.subTest(mode=oct(mode), service=service):
                        source.chmod(mode)
                        target = root / f"target-{mode}"
                        previous = os.umask(0o077)
                        try:
                            target.mkdir()
                            await UnixOps(env).download_dir(
                                str(source), target, service=service
                            )
                            self.assertEqual(os.umask(0o077), 0o077)
                        finally:
                            os.umask(previous)
                        self.assertEqual(stat.S_IMODE(target.stat().st_mode), mode)
                        for name, expected in [
                            ("run", 0o751), ("private", 0o700), ("private/data", 0o600)
                        ]:
                            self.assertEqual(
                                stat.S_IMODE((target / name).stat().st_mode), expected
                            )
                        self.assertEqual((target / "private/data").read_text(), "private")
            self.assertIs(UnixOps.download_dir, original)

    async def test_permission_lookup_failure_does_not_copy_artifacts(self):
        env = SimpleNamespace(
            service_exec=AsyncMock(return_value=SimpleNamespace(
                return_code=1, stdout="", stderr="not found"
            )),
            _run_docker_compose_command=AsyncMock(),
        )
        with preserve_download_permissions(), self.assertRaisesRegex(RuntimeError, "not found"):
            await UnixOps(env).download_dir("/missing", "/unused")
        env._run_docker_compose_command.assert_not_awaited()

    async def test_failed_copy_does_not_change_destination_permissions(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory)
            target.chmod(0o700)
            env = SimpleNamespace(
                service_exec=AsyncMock(return_value=SimpleNamespace(
                    return_code=0, stdout="755", stderr=""
                )),
                _run_docker_compose_command=AsyncMock(side_effect=RuntimeError("copy failed")),
            )
            with preserve_download_permissions(), self.assertRaisesRegex(RuntimeError, "copy failed"):
                await UnixOps(env).download_dir("/source", target)
            self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o700)

    def test_cli_applies_fix_and_restores_it_on_exit(self):
        from kraai_harbor.cli import main

        original = UnixOps.download_dir

        def app():
            self.assertIsNot(UnixOps.download_dir, original)
            raise SystemExit(0)

        with patch("harbor.cli.main.app", side_effect=app), self.assertRaises(SystemExit):
            main()
        self.assertIs(UnixOps.download_dir, original)
