import hashlib
import io
import json
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path


class Fixture(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="kraai-harbor-test-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.bundle = self.root / "runner.tar"
        with tarfile.open(self.bundle, "w") as bundle:
            item = tarfile.TarInfo("nix/store/example/bin/runner")
            item.size = 4
            item.mode = 0o755
            bundle.addfile(item, io.BytesIO(b"test"))
        self.spec_data = {
            "schema_version": 1,
            "harness": "kraai",
            "runner_bundle": str(self.bundle),
            "runner_path": "/nix/store/example/bin/runner",
            "runner_args": [
                "--message",
                "{prompt}",
                "--model",
                "{model}",
                "--provider-config",
                "{provider_config}",
            ],
            "model": "gpt-6-astra-low",
            "proxy_command": [sys.executable],
            "bundle_sha256": hashlib.sha256(self.bundle.read_bytes()).hexdigest(),
        }
        self.spec_path = self.root / "spec.json"
        self.write_spec()

    def write_spec(self):
        self.spec_path.write_text(json.dumps(self.spec_data))
