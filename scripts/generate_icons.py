"""Generate deterministic placeholder app icons (PNG/ICO/ICNS) with stdlib only.

The real brand artwork replaces these before release; the formats and sizes
match what tauri.conf.json references so `cargo build` succeeds from a fresh
clone without external tooling.
"""
import struct
import zlib
from pathlib import Path

OUT = Path(__file__).resolve().parent.parent / "src-tauri" / "icons"


def png_chunk(tag: bytes, data: bytes) -> bytes:
    return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)


def write_png(path: Path, size: int):
    # Simple two-tone diagonal gradient square, RGBA.
    rows = []
    for y in range(size):
        row = bytearray([0])  # filter: none
        for x in range(size):
            t = (x + y) / (2 * size)
            r = int(24 + 40 * t)
            g = int(26 + 120 * t)
            b = int(34 + 160 * t)
            # margin border transparent
            m = max(1, size // 16)
            if x < m or y < m or x >= size - m or y >= size - m:
                row += bytes((0, 0, 0, 0))
            else:
                row += bytes((r, g, b, 255))
        rows.append(bytes(row))
    raw = b"".join(rows)
    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    payload = (b"\x89PNG\r\n\x1a\n"
               + png_chunk(b"IHDR", ihdr)
               + png_chunk(b"IDAT", zlib.compress(raw, 9))
               + png_chunk(b"IEND", b""))
    path.write_bytes(payload)


def write_ico(path: Path):
    # ICO wrapping the 32px PNG (Vista+ PNG-in-ICO is valid).
    png = (OUT / "32x32.png").read_bytes()
    header = struct.pack("<HHH", 0, 1, 1)
    entry = struct.pack("<BBBBHHII", 32, 32, 0, 0, 1, 32, len(png), 6 + 16)
    path.write_bytes(header + entry + png)


def write_icns(path: Path):
    # Minimal modern icns: ic07(128) + ic08(256) PNG chunks.
    def chunk(tag: bytes, data: bytes) -> bytes:
        return tag + struct.pack(">I", len(data) + 8) + data
    body = (chunk(b"ic07", (OUT / "128x128.png").read_bytes())
            + chunk(b"ic08", (OUT / "128x128@2x.png").read_bytes()))
    path.write_bytes(b"icns" + struct.pack(">I", len(body) + 8) + body)


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    for size, name in [(32, "32x32.png"), (128, "128x128.png"), (256, "128x128@2x.png"), (512, "icon.png")]:
        write_png(OUT / name, size)
    write_ico(OUT / "icon.ico")
    write_icns(OUT / "icon.icns")
    print(f"wrote icons to {OUT}")


if __name__ == "__main__":
    main()
