"""Check tracked/staged source without printing potentially sensitive matches."""
import re
import ipaddress
import subprocess
import sys

RULES = {
    "private home/workspace path": r"(?i)(?:[a-z]:[\\/]+Users[\\/]+[^\s\"']+|[a-z]:[\\/]+Codebox[\\/]+|/ho" r"me/[^/\s]+/)",
    "IPv4 address": r"\b(?:\d{1,3}\.){3}\d{1,3}\b",
    "embedded SSH fingerprint": r"SHA256:[A-Za-z0-9+/]{30,}={0,2}",
    "GitHub credential": r"\b(?:gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{30,})\b",
    "private key": r"-----BEGIN (?:RSA |OPENSSH |EC )?PRIVATE KEY-----",
    "saved encrypted credential": r'"password_enc"\s*:\s*"[^"\s]+"',
    "email address": r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}",
}
FORBIDDEN = {".env", "config.json", "state.json", "stats.json"}


def main():
    staged = "--staged" in sys.argv
    files = subprocess.check_output(["git", "ls-files", "-z"]).decode().split("\0")
    if not any(files):
        print("FAIL: no tracked files to scan")
        return 1
    failures = []
    for name in filter(None, files):
        if name.split("/")[-1] in FORBIDDEN or name.startswith((".claude/", ".hbsync/", ".temp_files/", "target/", "dist/")):
            failures.append((name, "private/generated file must not be tracked"))
            continue
        data = subprocess.check_output(["git", "show", ":" + name]) if staged else open(name, "rb").read()
        if b"\0" in data:
            continue
        source = data.decode("utf-8-sig")
        for rule, pattern in RULES.items():
            if rule == "IPv4 address":
                for match in re.finditer(pattern, source):
                    try:
                        ipaddress.IPv4Address(match.group())
                    except ipaddress.AddressValueError:
                        continue
                    failures.append((name, rule))
                continue
            if re.search(pattern, source):
                failures.append((name, rule))
    for name, rule in failures:
        print(f"FAIL: {name}: {rule} (value redacted)")
    if failures:
        return 1
    print(f"PASS: privacy patterns checked in {len(list(filter(None, files)))} {'staged' if staged else 'tracked'} files; no matches.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
