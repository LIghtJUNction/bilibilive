"""Compile the project's small SVG icon to installer formats with no native tools.

This deliberately implements only the SVG shapes used by bilibilive.svg. An
unsupported edit fails the build instead of silently changing the icon. All
raster/ICO/ICNS outputs stay in the external installer staging directory.
"""
from __future__ import annotations
import binascii
import math
from pathlib import Path
import re
import struct
import xml.etree.ElementTree as ET
import zlib


def color(value):
    if not re.fullmatch(r"#[0-9a-fA-F]{6}", value):
        raise ValueError("Installer icon supports opaque hexadecimal fills only")
    return tuple(bytes.fromhex(value[1:])) + (255,)


def path_polygon(source):
    tokens = re.findall(r"[A-Za-z]|[-+]?(?:[0-9]*\.)?[0-9]+", source)
    if re.sub(r"[\s,]", "", source) != "".join(tokens):
        raise ValueError("Unsupported SVG path syntax")
    points = []
    current = (0.0, 0.0)
    first = None
    previous_control = None
    command = None
    index = 0
    while index < len(tokens):
        if tokens[index].isalpha():
            command = tokens[index]
            index += 1
        if command not in "MmLlHhVvCcSsZz":
            raise ValueError("Unsupported SVG path command")
        if command in "Zz":
            if first is not None: points.append(first)
            current = first
            previous_control = None
            command = None
            continue
        count = {"M": 2, "L": 2, "H": 1, "V": 1, "C": 6, "S": 4}[command.upper()]
        values = list(map(float, tokens[index:index + count]))
        if len(values) != count: raise ValueError("Incomplete SVG path")
        index += count
        x, y = current
        relative = command.islower()
        def point(a, b): return (a + x, b + y) if relative else (a, b)
        upper = command.upper()
        if upper in ("M", "L"):
            current = point(*values)
            points.append(current)
            if upper == "M":
                first = current
                command = "l" if relative else "L"
            previous_control = None
        elif upper == "H":
            current = (values[0] + x if relative else values[0], y)
            points.append(current); previous_control = None
        elif upper == "V":
            current = (x, values[0] + y if relative else values[0])
            points.append(current); previous_control = None
        else:
            if upper == "C":
                control1, control2, end = point(*values[:2]), point(*values[2:4]), point(*values[4:])
            else:
                control1 = (2 * x - previous_control[0], 2 * y - previous_control[1]) if previous_control else current
                control2, end = point(*values[:2]), point(*values[2:])
            start = current
            for i in range(1, 33):
                t = i / 32; u = 1 - t
                points.append(tuple(u**3 * start[k] + 3*u*u*t*control1[k] + 3*u*t*t*control2[k] + t**3*end[k] for k in (0, 1)))
            current = end; previous_control = control2
    return points


def shapes(svg):
    root = ET.fromstring(Path(svg).read_bytes())
    if root.attrib.get("viewBox") != "0 0 128 128":
        raise ValueError("Installer icon requires the 128-unit project viewBox")
    result = []
    for item in root:
        kind = item.tag.rsplit("}", 1)[-1]
        attrs = item.attrib
        if kind == "path":
            if set(attrs) != {"d", "fill"}: raise ValueError("Unsupported SVG path attributes")
            points = path_polygon(attrs["d"])
        elif kind == "circle":
            if set(attrs) != {"cx", "cy", "r", "fill"}: raise ValueError("Unsupported SVG circle attributes")
            cx, cy, r = map(float, (attrs["cx"], attrs["cy"], attrs["r"]))
            points = [(cx + r*math.cos(i*math.tau/128), cy + r*math.sin(i*math.tau/128)) for i in range(128)]
        elif kind == "rect":
            if set(attrs) != {"width", "height", "rx", "fill"}: raise ValueError("Unsupported SVG rectangle attributes")
            width, height, radius = map(float, (attrs["width"], attrs["height"], attrs["rx"]))
            points = []
            for cx, cy, start in ((width-radius, radius, -90), (width-radius, height-radius, 0),
                                  (radius, height-radius, 90), (radius, radius, 180)):
                for i in range(33):
                    angle = math.radians(start + 90*i/32)
                    points.append((cx + radius*math.cos(angle), cy + radius*math.sin(angle)))
        else:
            raise ValueError(f"Unsupported SVG shape: {kind}")
        if len(points) < 3: raise ValueError("Empty SVG polygon")
        result.append((points, color(attrs["fill"])))
    return result


def png_image(shapes_, size):
    scale = 2
    width = size * scale
    pixels = bytearray(width * width * 4)
    for polygon, fill in shapes_:
        points = [(x*width/128, y*width/128) for x, y in polygon]
        edges = list(zip(points, points[1:] + points[:1]))
        for row in range(width):
            y = row + 0.5
            crosses = sorted((x1 + (y-y1)*(x2-x1)/(y2-y1), 1 if y2 > y1 else -1)
                             for (x1,y1),(x2,y2) in edges if (y1 <= y < y2) or (y2 <= y < y1))
            winding = 0
            left = 0.0
            for right, direction in crosses:
                if winding:
                    start, stop = max(0, math.ceil(left-0.5)), min(width, math.ceil(right-0.5))
                    if stop > start:
                        offset = (row*width + start)*4
                        pixels[offset:offset + (stop-start)*4] = bytes(fill)*(stop-start)
                winding += direction
                left = right
    raw = bytearray()
    for row in range(size):
        raw.append(0)
        for column in range(size):
            positions = [((row*2+y)*width + column*2+x)*4 for y in (0,1) for x in (0,1)]
            alpha_sum = sum(pixels[p+3] for p in positions)
            for c in range(3):
                raw.append(round(sum(pixels[p+c]*pixels[p+3] for p in positions)/alpha_sum) if alpha_sum else 0)
            raw.append(round(alpha_sum/4))
    def chunk(kind, value):
        return struct.pack(">I", len(value)) + kind + value + struct.pack(">I", binascii.crc32(kind + value) & 0xffffffff)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", size,size,8,6,0,0,0))
            + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def create_icons(svg, output):
    output = Path(output); output.mkdir(parents=True)
    drawing = shapes(svg)
    images = {size: png_image(drawing, size) for size in (16, 32, 48, 128, 256, 512)}
    sizes = (16, 32, 48, 256)
    offset = 6 + 16 * len(sizes)
    ico = bytearray(struct.pack("<HHH", 0, 1, len(sizes)))
    for size in sizes:
        image = images[size]
        ico.extend(struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(image), offset))
        offset += len(image)
    for size in sizes: ico.extend(images[size])
    (output / "bilibilive.ico").write_bytes(ico)
    blocks = b"".join(kind + struct.pack(">I", 8 + len(images[size])) + images[size]
                      for kind, size in ((b"ic07",128),(b"ic08",256),(b"ic09",512)))
    (output / "bilibilive.icns").write_bytes(b"icns" + struct.pack(">I", 8 + len(blocks)) + blocks)
    (output / "bilibilive.png").write_bytes(images[256])
    return output
