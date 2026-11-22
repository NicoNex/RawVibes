#!/usr/bin/env python3
"""Converts the lensfun database (XML, CC-BY-SA 3.0) into the JSON RawVibes reads.

    tools/lensfun_db.py <xml dir> <out.json> [--only "maker|model" ...]

Defaults follow lensfun 0.3.4's database reader (database.cpp): a lens is
rectilinear with aspect ratio 1.5 unless it says otherwise; TCA terms start at 1.
`--only` keeps the named lenses (and every camera and mount): the test fixture.
"""

import json
import sys
import xml.etree.ElementTree as ET
from pathlib import Path


def text(el, tag):
    """The element's `tag` child in no particular language (lensfun's default name)."""
    plain = [c for c in el.findall(tag) if "lang" not in c.attrib]
    found = plain or el.findall(tag)
    return (found[0].text or "").strip() if found else ""


def num(v, default=0.0):
    try:
        return float(v)
    except (TypeError, ValueError):
        return default


def aspect(s):
    if not s:
        return 1.5
    if ":" in s:
        a, b = s.split(":", 1)
        return num(a) / num(b, 1.0)
    return num(s, 1.5)


def calibration(lens_el):
    dist, tca, vig, real = [], [], [], []
    for cal in lens_el.findall("calibration"):
        for d in cal.findall("distortion"):
            a = d.attrib
            k = [num(a.get("a", a.get("k1"))), num(a.get("b", a.get("k2"))), num(a.get("c"))]
            dist.append({"model": a.get("model", "none"), "focal": num(a.get("focal")), "k": k})
        for t in cal.findall("tca"):
            a = t.attrib
            # lensfun order: kr|vr, kb|vb, cr, cb, br, bb (the first two default to 1).
            k = [
                num(a.get("kr", a.get("vr")), 1.0),
                num(a.get("kb", a.get("vb")), 1.0),
                num(a.get("cr")),
                num(a.get("cb")),
                num(a.get("br")),
                num(a.get("bb")),
            ]
            tca.append({"model": a.get("model", "none"), "focal": num(a.get("focal")), "k": k})
        for v in cal.findall("vignetting"):
            a = v.attrib
            vig.append({
                "model": a.get("model", "none"),
                "focal": num(a.get("focal")),
                "aperture": num(a.get("aperture")),
                "distance": num(a.get("distance")),
                "k": [num(a.get("k1")), num(a.get("k2")), num(a.get("k3"))],
            })
        for r in cal.findall("real-focal-length"):
            real.append([num(r.get("focal")), num(r.get("real-focal"))])
    return dist, tca, vig, real


def convert(xml_dir, only=None):
    db = {"mounts": [], "cameras": [], "lenses": []}
    for path in sorted(Path(xml_dir).glob("*.xml")):
        root = ET.parse(path).getroot()
        for m in root.findall("mount"):
            db["mounts"].append({"name": text(m, "name"), "compat": [(c.text or "").strip() for c in m.findall("compat")]})
        for c in root.findall("camera"):
            db["cameras"].append({
                "maker": text(c, "maker"),
                "model": text(c, "model"),
                "variant": text(c, "variant"),
                "mount": text(c, "mount"),
                "crop": num(text(c, "cropfactor")),
            })
        for l in root.findall("lens"):
            maker, model = text(l, "maker"), text(l, "model")
            if only is not None and f"{maker}|{model}" not in only:
                continue
            dist, tca, vig, real = calibration(l)
            centre = l.find("center")
            db["lenses"].append({
                "maker": maker,
                "model": model,
                "mounts": [(m.text or "").strip() for m in l.findall("mount")],
                "crop": num(text(l, "cropfactor")),
                "aspect": aspect(text(l, "aspect-ratio")),
                "type": text(l, "type") or "rectilinear",
                "center": [num(centre.get("x")), num(centre.get("y"))] if centre is not None else [0.0, 0.0],
                "dist": dist,
                "tca": tca,
                "vig": vig,
                "real_focal": real,
            })
    return db


def main(argv):
    if len(argv) < 3:
        sys.exit(__doc__)
    only = None
    if "--only" in argv:
        i = argv.index("--only")
        only = set(argv[i + 1:])
        argv = argv[:i]
    db = convert(argv[1], only)
    Path(argv[2]).parent.mkdir(parents=True, exist_ok=True)
    Path(argv[2]).write_text(json.dumps(db, separators=(",", ":")))
    print(f"{len(db['lenses'])} lenses, {len(db['cameras'])} cameras -> {argv[2]}")


if __name__ == "__main__":
    main(sys.argv)
