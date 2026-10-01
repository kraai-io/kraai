from contextlib import contextmanager
from functools import wraps
from pathlib import Path
import shlex

from harbor.environments.docker.docker_unix import UnixOps


@contextmanager
def preserve_download_permissions():
    original = UnixOps.download_dir

    @wraps(original)
    async def download_dir(self, source_dir, target_dir, service=None):
        result = await self._env.service_exec(
            f"stat -L -c %a -- {shlex.quote(source_dir)}",
            service=service,
            user="root",
            timeout_sec=10,
        )
        if result.return_code != 0:
            raise RuntimeError(
                f"Could not read artifact directory permissions for {source_dir!r}: "
                f"{result.stderr or result.stdout}"
            )
        mode = int(result.stdout.strip(), 8)
        if not 0 <= mode <= 0o7777:
            raise ValueError(f"Invalid directory mode: {result.stdout!r}")
        await original(self, source_dir, target_dir, service=service)
        Path(target_dir).chmod(mode, follow_symlinks=False)

    UnixOps.download_dir = download_dir
    try:
        yield
    finally:
        UnixOps.download_dir = original


def main():
    from harbor.cli.main import app

    with preserve_download_permissions():
        app()


if __name__ == "__main__":
    main()
