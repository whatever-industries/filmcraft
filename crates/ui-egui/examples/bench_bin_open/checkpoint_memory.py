"""Measure first-checkpoint RSS on macOS. Give a bench_bin_open binary, a modular
manifest, its discovery review JSON file, and a trial count (after one warmup).
The input project must be a disposable copy with its reviewed media available.
"""
import argparse
import ctypes
import hashlib
import json
import os
from pathlib import Path
import selectors
import subprocess
import tempfile
import time


def rss(pid):
    # proc_taskinfo starts with virtual_size and resident_size (both uint64_t).
    buf = (ctypes.c_uint64 * 16)()
    if libproc.proc_pidinfo(pid, 4, 0, buf, ctypes.sizeof(buf)) < 16:
        raise OSError("unable to read task resident size")
    return buf[1]


def signal_arm(proc, message):
    proc.stdin.write((message + "\n").encode())
    proc.stdin.flush()


def pair(binary, manifest, review, index, trial):
    with tempfile.TemporaryDirectory() as directory:
        arms = {}
        try:
            for name in (("control", "writer") if trial % 2 == 0 else ("writer", "control")):
                root = Path(directory) / name
                key = hashlib.sha256(os.fsencode(manifest.parent)).hexdigest()
                cache = root / "Preview indexes" / key
                cache.mkdir(parents=True)
                (cache / "index.json").write_bytes(index)
                proc = subprocess.Popen(
                    [binary, "relink-desktop", manifest, review, "checkpoint-pair"],
                    env=os.environ | {"FILMCRAFT_DATA_DIR": str(root)},
                    stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                )
                arms[name] = proc
                if proc.stdout.readline().strip() != b"ready":
                    raise RuntimeError("benchmark arm failed before Apply")
            writer, control = arms["writer"], arms["control"]
            peaks = {name: rss(p.pid) for name, p in arms.items()}
            extra = peaks["writer"] - peaks["control"]
            with selectors.DefaultSelector() as selector:
                selector.register(writer.stdout, selectors.EVENT_READ)
                signal_arm(writer, "flush")
                deadline = time.monotonic() + 120
                while True:
                    values = {name: rss(p.pid) for name, p in arms.items()}
                    for name, value in values.items():
                        peaks[name] = max(peaks[name], value)
                    extra = max(extra, values["writer"] - values["control"])
                    if selector.select(0.001):
                        row = json.loads(writer.stdout.readline())
                        break
                    if time.monotonic() > deadline:
                        raise TimeoutError("checkpoint did not finish")
            retained = {name: rss(p.pid) for name, p in arms.items()}
            for name, value in retained.items():
                peaks[name] = max(peaks[name], value)
            extra = max(extra, retained["writer"] - retained["control"])
            lifetime = {}
            for name, proc in arms.items():
                signal_arm(proc, "finish")
                _, status, usage = os.wait4(proc.pid, 0)
                proc.returncode = os.waitstatus_to_exitcode(status)
                if proc.returncode:
                    raise RuntimeError(f"{name} exited with {proc.returncode}")
                lifetime[name] = usage.ru_maxrss  # bytes on macOS
            return row | dict(trial=trial, extraBytes=extra, phasePeakBytes=peaks,
                              retainedBytes=retained, lifetimePeakBytes=lifetime)
        finally:
            for proc in arms.values():
                if proc.poll() is None:
                    proc.kill()
                    proc.wait()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("manifest", type=Path)
    parser.add_argument("review", type=Path)
    parser.add_argument("trials", type=int)
    args = parser.parse_args()
    libproc = ctypes.CDLL("/usr/lib/libproc.dylib")
    manifest = args.manifest.resolve()
    with tempfile.TemporaryDirectory() as scratch:
        path = Path(scratch) / "index.json"
        subprocess.run([args.binary, "index", manifest, path], stdout=subprocess.DEVNULL, check=True)
        index = path.read_bytes()
    for trial in range(-1, args.trials):
        print(json.dumps(pair(args.binary, manifest, args.review.read_text(), index, trial)), flush=True)
