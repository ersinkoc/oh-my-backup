"""Verify release checksum, archive privacy, PE icon resources and binary startup."""
import hashlib
import pathlib
import re
import struct
import subprocess
import tempfile
import zipfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
VERSION = re.search(r'^version = "([0-9.]+)"', (ROOT / "Cargo.toml").read_text(), re.M)[1]
PACKAGE = ROOT / "dist" / ("v" + VERSION)
ARCHIVE = PACKAGE / f"oh-my-backup-v{VERSION}-windows-x64.zip"


def verify_pe(data):
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    assert data[pe:pe + 4] == b"PE\0\0"
    section_count = struct.unpack_from("<H", data, pe + 6)[0]
    opt_size = struct.unpack_from("<H", data, pe + 20)[0]
    opt = pe + 24
    assert struct.unpack_from("<H", data, opt)[0] == 0x20B, "expected x64 PE"
    sections = []
    for i in range(section_count):
        start = opt + opt_size + i * 40
        virtual_size, rva, size, offset = struct.unpack_from("<IIII", data, start + 8)
        sections.append((rva, max(virtual_size, size), offset))

    def offset(rva):
        for base, size, off in sections:
            if base <= rva < base + size:
                return off + rva - base
        raise AssertionError("invalid resource address")

    resources_rva = struct.unpack_from("<I", data, opt + 112 + 16)[0]
    base = offset(resources_rva)

    def entries(relative):
        start = base + relative
        named, ids = struct.unpack_from("<HH", data, start + 12)
        return dict(struct.unpack_from("<II", data, start + 16 + i * 8) for i in range(named + ids))

    root = entries(0)
    assert {3, 14, 16}.issubset(root), "executable must embed icons, an icon group and version information"
    icons = entries(root[3] & 0x7FFFFFFF)
    assert len(icons) == 9, "expected nine embedded icon sizes"
    for item in icons.values():
        language = next(iter(entries(item & 0x7FFFFFFF).values()))
        rva, length = struct.unpack_from("<II", data, base + language)
        assert data[offset(rva):offset(rva) + 8] == b"\x89PNG\r\n\x1a\n"
        assert length > 0
    for pattern in [rb"(?i)[a-z]:\\Users\\", rb"(?i)[a-z]:\\Codebox\\"]:
        assert not re.search(pattern, data), "release embeds a private build path"
        assert not re.search(pattern, data.replace(b"\0", b"")), "release embeds a UTF-16 private build path"


def main():
    checksum, name = (PACKAGE / "SHA256SUMS.txt").read_text().strip().split()
    assert name == ARCHIVE.name
    assert hashlib.sha256(ARCHIVE.read_bytes()).hexdigest() == checksum
    expected = {"oh-my-backup.exe", "install.ps1", "README.md", "CHANGELOG.md", "hbsync.ico", "logo.svg"}
    with zipfile.ZipFile(ARCHIVE) as archive:
        files = [f for f in archive.infolist() if not f.is_dir()]
        assert {pathlib.PurePosixPath(f.filename).name for f in files} == expected
        for f in files:
            path = pathlib.PurePosixPath(f.filename)
            assert not path.is_absolute() and ".." not in path.parts
        executable = next(f for f in files if f.filename.endswith("/oh-my-backup.exe"))
        data = archive.read(executable)
        verify_pe(data)
        with tempfile.TemporaryDirectory(prefix="oh-my-backup-package-") as directory:
            binary = pathlib.Path(directory, "oh-my-backup.exe")
            binary.write_bytes(data)
            result = subprocess.run([str(binary), "--version"], capture_output=True, text=True, timeout=20, check=True)
            assert result.stdout.strip() == f"Oh My Backup {VERSION}"
    print(f"PASS: v{VERSION} checksum, archive contents, x64 PE, nine icon resources, private build-path removal and --version startup")


if __name__ == "__main__":
    main()
