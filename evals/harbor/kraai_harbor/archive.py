import posixpath
import tarfile
from pathlib import Path, PurePosixPath


def validate_bundle(path: Path) -> None:
    with tarfile.open(path) as bundle:
        for member in bundle:
            name = PurePosixPath(member.name)
            if name.is_absolute() or ".." in name.parts:
                raise ValueError(f"Unsafe runner bundle path: {member.name}")
            if not (
                name.is_relative_to("nix/store")
                or name.is_relative_to("installed-agent/bin")
            ):
                raise ValueError(f"Unexpected runner bundle path: {member.name}")
            if not (
                member.isfile() or member.isdir() or member.issym() or member.islnk()
            ):
                raise ValueError(f"Unsupported runner bundle member: {member.name}")
            if member.issym():
                target = PurePosixPath(
                    posixpath.normpath(
                        posixpath.join("/", str(name.parent), member.linkname)
                    )
                )
                if not (
                    target.is_relative_to("/nix/store")
                    or target.is_relative_to("/installed-agent/bin")
                ):
                    raise ValueError(
                        f"Unsafe runner bundle symbolic link: {member.name}"
                    )
            if member.islnk():
                target = PurePosixPath(member.linkname)
                if (
                    target.is_absolute()
                    or ".." in target.parts
                    or not target.is_relative_to("nix/store")
                ):
                    raise ValueError(f"Unsafe runner bundle hard link: {member.name}")
