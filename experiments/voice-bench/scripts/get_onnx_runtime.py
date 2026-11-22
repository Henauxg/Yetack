# Downloads Microsoft's onnxruntime wheel from PyPI, checks PyPI's SHA-256, and copies out the DLLs.
import hashlib, json, sys, urllib.request, zipfile, pathlib
# Usage: get_onnx_runtime.py <version> <folder for the wheel> <folder for the DLLs>
version, wheel_dir, dll_dir = sys.argv[1], pathlib.Path(sys.argv[2]), pathlib.Path(sys.argv[3])
wheel_dir.mkdir(parents=True, exist_ok=True)
dll_dir.mkdir(parents=True, exist_ok=True)
meta = json.load(urllib.request.urlopen(f"https://pypi.org/pypi/onnxruntime/{version}/json"))
f = next(u for u in meta["urls"] if u["filename"].endswith("cp313-cp313-win_amd64.whl"))
path = wheel_dir / f["filename"]
if not path.exists():
    urllib.request.urlretrieve(f["url"], path)
digest = hashlib.sha256(path.read_bytes()).hexdigest()
assert digest == f["digests"]["sha256"], (digest, f["digests"]["sha256"])
print("wheel", f["filename"], "sha256 ok", digest)
with zipfile.ZipFile(path) as z:
    for name in z.namelist():
        if name.startswith("onnxruntime/capi/") and name.endswith(".dll"):
            target = dll_dir / pathlib.Path(name).name
            target.write_bytes(z.read(name))
            print("dll", target.name, target.stat().st_size)
