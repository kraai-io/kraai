import hashlib
import json
import re
from dataclasses import dataclass
from pathlib import Path, PurePosixPath


@dataclass(frozen=True)
class RunnerSpec:
    harness: str
    runner_bundle: Path
    runner_path: str
    runner_args: tuple[str, ...]
    model: str
    proxy_command: tuple[str, ...]
    bundle_sha256: str

    @classmethod
    def load(cls, path: str | Path) -> "RunnerSpec":
        data = json.loads(Path(path).read_text())
        expected = {"schema_version", *cls.__dataclass_fields__}
        if not isinstance(data, dict) or set(data) - expected:
            raise ValueError("Runner spec must be an object with known fields")
        if data.pop("schema_version", None) != 1:
            raise ValueError("Unsupported runner spec schema version")
        for key in ("harness", "runner_path", "model", "bundle_sha256"):
            if not isinstance(data.get(key), str) or not data[key] or "\0" in data[key]:
                raise ValueError(f"Runner spec requires a nonempty {key}")
        for key in ("runner_args", "proxy_command"):
            if not isinstance(data.get(key), list) or any(
                not isinstance(item, str) or "\0" in item for item in data[key]
            ):
                raise ValueError(f"Runner spec requires a string list for {key}")
            data[key] = tuple(data[key])
        if not any("{prompt}" in arg for arg in data["runner_args"]):
            raise ValueError("Runner arguments must include {prompt}")
        if (
            not data["proxy_command"]
            or not Path(data["proxy_command"][0]).is_absolute()
        ):
            raise ValueError("Proxy command must name an absolute host executable")
        if not re.fullmatch(r"[a-f0-9]{64}", data["bundle_sha256"]):
            raise ValueError("Runner bundle requires a SHA-256 digest")
        runner_path = PurePosixPath(data["runner_path"])
        if not runner_path.is_absolute() or ".." in runner_path.parts:
            raise ValueError("Runner path must be absolute without parent traversal")
        if not (
            runner_path.is_relative_to("/nix/store")
            or runner_path.is_relative_to("/installed-agent/bin")
        ):
            raise ValueError("Runner must be in the installed bundle")
        data["runner_bundle"] = Path(data["runner_bundle"]).resolve(strict=True)
        with data["runner_bundle"].open("rb") as bundle:
            if (
                hashlib.file_digest(bundle, "sha256").hexdigest()
                != data["bundle_sha256"]
            ):
                raise ValueError("Runner bundle SHA-256 does not match the spec")
        return cls(**data)

    def arguments(
        self,
        instruction: str,
        workspace: str,
        proxy_url: str,
        provider_id: str,
        provider_config: str,
    ) -> list[str]:
        replacements = {
            "prompt": instruction,
            "model": self.model,
            "workspace": workspace,
            "proxy_url": proxy_url,
            "provider_id": provider_id,
            "provider_config": provider_config,
        }
        pattern = re.compile(
            r"\{(prompt|model|workspace|proxy_url|provider_id|provider_config)\}"
        )
        return [
            pattern.sub(lambda match: replacements[match[1]], arg)
            for arg in self.runner_args
        ]
