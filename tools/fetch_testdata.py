"""Download test files that are not stored in the repo (unclear or copyleft license) into testdata/external/."""
import hashlib
import urllib.parse
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TARGET = ROOT / "testdata" / "external"

# (repository, commit, path in repository, sha256)
FILES = [
    # Robe MegaPointe with 7 wheels, prism facets and 3DS models; spec repository (package.json: ISC)
    ("mvrdevelopment/spec", "098d3791f77f0895bd859adf01864b4826e2006f",
     "proposal/mvr-update-for-gdtf12/basic_gdtf.mvr", "330ab32a4c73499fc328a33965019e1532c0cedb7035e330bb90bd8e154e5c15"),
    # two gobo wheels with images; BlenderDMX (GPL-3.0)
    ("open-stage/blender-dmx", "27993b20466f3fff34ff95debfe7c902a414fed8",
     "assets/profiles/BlenderDMX@BeskydBeam@version10.gdtf", "7b9787ae6bc7de83eb9d4b4814064f5762913c3bfbe4dc51c3e0c3c093117fe4"),
]


def main():
    TARGET.mkdir(parents=True, exist_ok=True)
    for repo, commit, path, sha256 in FILES:
        dest = TARGET / Path(path).name
        if dest.exists():
            print(f"have {dest.name}")
            continue
        url = f"https://raw.githubusercontent.com/{repo}/{commit}/{urllib.parse.quote(path)}"
        data = urllib.request.urlopen(url).read()
        if sha256 and hashlib.sha256(data).hexdigest() != sha256:
            raise SystemExit(f"{dest.name}: checksum mismatch")
        dest.write_bytes(data)
        print(f"fetched {dest.name} ({len(data)} bytes)")


if __name__ == "__main__":
    main()
