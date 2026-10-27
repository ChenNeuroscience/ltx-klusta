"""Shared helpers: running ltxk, writing synthetic recordings, and reading .npy dumps."""
import ast
import random
import struct
import subprocess


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def command(exe, *args, env=None, timeout=120):
    return subprocess.run([str(exe), *map(str, args)], capture_output=True, text=True,
                          encoding="utf-8", timeout=timeout, env=env, stdin=subprocess.DEVNULL)


def write_waves(path, waves, active=1):
    """One spike per entry; each entry lists 50 samples for each of `active` channels."""
    payload = bytearray()
    for index, channels in enumerate(waves):
        channels = channels if active > 1 else [channels]
        for channel in range(4):
            wave = channels[channel] if channel < active else [0] * 50
            payload += struct.pack("<I50b", index, *wave)
    path.write_bytes(f"num_spikes {len(waves)}\r\ndata_start".encode() + payload)


def recording(path, spikes=1200, active_channels=1):
    """Two well-separated populations, alternating spike by spike."""
    rng = random.Random(4637)
    data = bytearray(f"num_spikes {spikes}\r\ndata_start".encode())
    for i in range(spikes):
        for channel in range(active_channels):
            latent = [(-45 if i % 2 == 0 else 45) * (1 - 0.12 * channel) + rng.gauss(0, 4),
                      rng.gauss(0, 6), rng.gauss(0, 6)]
            wave = [max(-127, min(127, round(latent[0 if j < 15 else 1 if j < 35 else 2] + rng.gauss(0, 1))))
                    for j in range(50)]
            data += struct.pack("<I50b", i, *wave)
        data += bytes((4 - active_channels) * 54)
    path.write_bytes(data)


NPY_TYPES = {"|u1": "B", "<u4": "I", "<f4": "f", "<f8": "d"}


def npy_header(stream):
    """Read an .npy header; return (struct format character, shape)."""
    require(stream.read(8) == b"\x93NUMPY\x01\x00", "Not a version 1.0 .npy file")
    length = struct.unpack("<H", stream.read(2))[0]
    header = ast.literal_eval(stream.read(length).decode("latin1"))
    require(not header["fortran_order"], "Unexpected Fortran-ordered array")
    return NPY_TYPES[header["descr"]], header["shape"]


def read_npy(path):
    """Return (shape, values) with values flattened in row-major order."""
    with open(path, "rb") as stream:
        code, shape = npy_header(stream)
        count = 1
        for n in shape:
            count *= n
        values = struct.unpack(f"<{count}{code}", stream.read())
    return shape, list(values)


def read_rows(path):
    """A 2-D .npy array as a list of rows."""
    (rows, columns), values = read_npy(path)
    return [values[r * columns:(r + 1) * columns] for r in range(rows)]


def dump_features(exe, recording_path, out_dir, features="3PC", extra_args=(), ok=True):
    """Run feature extraction only; return the rows, or stderr on expected failure."""
    result = command(exe, "cluster", recording_path, "--no-queue", "--features", features,
                     "--dump", "pca,features", "--dump-only", "--dump-dir", out_dir, *extra_args)
    require((result.returncode == 0) == ok, result.stdout + result.stderr)
    if not ok:
        return result.stderr
    return read_rows(out_dir / recording_path.name / "features.npy")
