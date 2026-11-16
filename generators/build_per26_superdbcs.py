#!/usr/bin/env python3
"""
Builds historical SuperDBC v1.1 JSON artifacts for all PER26 / PER25 track days
from git commits and .dbc files so every dataset in PER26/ decodes and enriches
with exact message/signal metadata, units, nominal periods, descriptions, and enums.
"""

import copy
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
SUPERDBC_DIR = REPO_ROOT / "superdbc"
PER26_DIR = REPO_ROOT / "PER26"


def compute_content_hash(doc: dict) -> str:
    payload = {k: v for k, v in doc.items() if k != "content_hash"}
    canonical = json.dumps(payload, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return hashlib.sha256(canonical).hexdigest()


def write_superdbc(path: Path, version_hash: str, buses: dict) -> None:
    doc = {
        "content_hash": "",
        "versions": {
            "schema_version": "1.1",
            "hash": version_hash,
        },
        "buses": buses,
    }
    doc["content_hash"] = compute_content_hash(doc)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(doc, indent=2) + "\n", encoding="utf-8")
    total_msgs = sum(len(b["messages"]) for b in buses.values())
    total_sigs = sum(len(m["signals"]) for b in buses.values() for m in b["messages"])
    print(f"Wrote {path.name}: {len(buses)} buses, {total_msgs} messages, {total_sigs} signals")


def raw_type_for_c_type(c_type: str, is_signed: bool) -> str:
    if c_type in ("float", "double"):
        return "float32"
    return "signed" if is_signed else "unsigned"


def default_c_type(bit_length: int, is_signed: bool, is_float: bool, has_choices: bool) -> str:
    if is_float:
        return "float"
    if bit_length == 1 and not is_signed:
        return "bool"
    prefix = "int" if is_signed else "uint"
    if bit_length <= 8:
        return f"{prefix}8_t"
    if bit_length <= 16:
        return f"{prefix}16_t"
    if bit_length <= 32:
        return f"{prefix}32_t"
    return f"{prefix}64_t"


def parse_dbc_file(dbc_path: Path, bus_id: int, baud_rate: int = 500000, period_lookup: dict | None = None) -> dict:
    """Parse a Vector DBC file into a SuperDBC v1.1 bus model."""
    text = dbc_path.read_text(encoding="utf-8", errors="replace")
    nodes = []
    messages_by_id = {}
    msg_order = []

    bo_re = re.compile(r"^BO_\s+(\d+)\s+(\w+)\s*:\s*(\d+)\s+(\w+)")
    sg_re = re.compile(
        r"^\s*SG_\s+(\w+)\s*:\s*(\d+)\|(\d+)@([01])([+-])\s*\(([^,]+),([^)]+)\)\s*\[([^|]+)\|([^\]]+)\]\s*\"([^\"]*)\"\s*(.+)$"
    )
    cm_bo_re = re.compile(r'^CM_\s+BO_\s+(\d+)\s+"([^"]*)"\s*;', re.MULTILINE)
    cm_sg_re = re.compile(r'^CM_\s+SG_\s+(\d+)\s+(\w+)\s+"([^"]*)"\s*;', re.MULTILINE)
    val_re = re.compile(r"^VAL_\s+(\d+)\s+(\w+)\s+(.+?)\s*;$", re.MULTILINE)
    val_pair_re = re.compile(r'(-?\d+)\s+"([^"]*)"')

    current_msg = None
    for raw_line in text.splitlines():
        line = raw_line.strip()
        if line.startswith("BU_:"):
            parts = line[4:].strip().split()
            for n in parts:
                if n and n != "Vector__XXX":
                    nodes.append({"name": n, "is_external": False})
            continue

        m_bo = bo_re.match(raw_line)
        if m_bo:
            raw_id = int(m_bo.group(1))
            is_ext = (raw_id & 0x80000000) != 0
            can_id = raw_id & 0x1FFFFFFF if is_ext else raw_id & 0x7FF
            msg_name = m_bo.group(2)
            dlc = min(8, max(1, int(m_bo.group(3))))
            tx = m_bo.group(4)
            if tx == "Vector__XXX":
                tx = "EXTERNAL"
            nom_period = (period_lookup or {}).get(msg_name, 10 if "CT3" in tx else 100)
            if nom_period is not None and nom_period <= 0:
                nom_period = None
            current_msg = {
                "id": can_id,
                "is_extended_id": is_ext,
                "message_name": msg_name,
                "transmitter": tx,
                "receivers": [],
                "length_bytes": dlc,
                "nominal_period_ms": nom_period,
                "priority": 3,
                "description": "",
                "signals": [],
                "_raw_id": raw_id,
            }
            messages_by_id[raw_id] = current_msg
            msg_order.append(current_msg)
            continue

        m_sg = sg_re.match(raw_line)
        if m_sg and current_msg is not None:
            sig_name = m_sg.group(1)
            start_bit = int(m_sg.group(2))
            bit_len = int(m_sg.group(3))
            byte_order = "little_endian" if m_sg.group(4) == "1" else "big_endian"
            if byte_order == "big_endian" and (start_bit % 8 == 0) and bit_len >= 8:
                start_bit += 7
            is_signed = m_sg.group(5) == "-"
            scale = float(m_sg.group(6))
            offset = float(m_sg.group(7))
            if scale == 0.0:
                scale = 1.0
            unit = m_sg.group(10).strip()
            rx_list = [r.strip() for r in m_sg.group(11).split(",") if r.strip() and r.strip() != "Vector__XXX"]
            for r in rx_list:
                if r not in current_msg["receivers"]:
                    current_msg["receivers"].append(r)
            c_type = default_c_type(bit_len, is_signed, False, False)
            choices = {"0": "OFF", "1": "ON"} if (bit_len == 1 and not is_signed) else None
            current_msg["signals"].append({
                "signal_name": sig_name,
                "description": "",
                "data_type": c_type,
                "raw_type": raw_type_for_c_type(c_type, is_signed),
                "start_bit": start_bit,
                "bit_length": bit_len,
                "byte_order": byte_order,
                "scale": scale,
                "offset": offset,
                "limits": None,
                "unit": unit,
                "choices": choices,
                "display_format": None,
            })

    for m_cm in cm_bo_re.finditer(text):
        raw_id = int(m_cm.group(1))
        if raw_id in messages_by_id:
            messages_by_id[raw_id]["description"] = m_cm.group(2).strip()

    for m_sg_cm in cm_sg_re.finditer(text):
        raw_id = int(m_sg_cm.group(1))
        sig_name = m_sg_cm.group(2)
        desc = m_sg_cm.group(3).strip()
        if raw_id in messages_by_id:
            for s in messages_by_id[raw_id]["signals"]:
                if s["signal_name"] == sig_name:
                    s["description"] = desc

    for m_val in val_re.finditer(text):
        raw_id = int(m_val.group(1))
        sig_name = m_val.group(2)
        rest = m_val.group(3)
        choices = {}
        for k, v in val_pair_re.findall(rest):
            if int(k) >= 0 and v.strip():
                choices[str(int(k))] = v.strip()
        if choices and raw_id in messages_by_id:
            for s in messages_by_id[raw_id]["signals"]:
                if s["signal_name"] == sig_name:
                    s["choices"] = choices

    for m in msg_order:
        m.pop("_raw_id", None)
        m["signals"].sort(key=lambda s: s["start_bit"])

    if not nodes:
        seen = sorted({m["transmitter"] for m in msg_order})
        nodes = [{"name": n, "is_external": True} for n in seen]

    return {
        "bus_id": bus_id,
        "baud_rate": baud_rate,
        "nodes": nodes,
        "messages": msg_order,
    }


def extract_git_tree(commit: str, subpath: str, dest: Path) -> None:
    files = subprocess.check_output(
        ["git", "ls-tree", "-r", "--name-only", commit, subpath]
    ).decode().splitlines()
    for f in files:
        rel = os.path.relpath(f, subpath)
        target = dest / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        content = subprocess.check_output(["git", "show", f"{commit}:{f}"])
        target.write_bytes(content)


def build_from_canpiler_commit(commit: str, root_subpath: str) -> tuple[dict, dict]:
    """Run historical canpiler in a subprocess to extract full SystemContext and period lookup."""
    with tempfile.TemporaryDirectory() as tmpdir:
        tmp_path = Path(tmpdir)
        extract_git_tree(commit, root_subpath, tmp_path)
        canpiler_dir = tmp_path / "canpiler"
        helper_code = """
import json, sys, types
sys.modules["jinja2"] = types.SimpleNamespace(
    Environment=lambda *a, **k: None,
    FileSystemLoader=lambda *a, **k: None,
    select_autoescape=lambda *a, **k: None,
)
sys.path.insert(0, ".")
from parser import parse_all, load_custom_types, create_system_context, load_bus_configs
from faultgen import augment_system_with_faults
from linker import link_all
from mapper import map_hardware

nodes = parse_all()
bus_configs = load_bus_configs()
custom_types = load_custom_types()
augment_system_with_faults(nodes, bus_configs, custom_types)
link_all(nodes)
mappings = map_hardware(nodes, bus_configs)
context = create_system_context(nodes, mappings, bus_configs, custom_types)

bus_id_map = {"VCAN": 0, "MCAN": 1, "CCAN": 2, "SCAN": 3, "GCAN": 4, "TCAN": 5}
out_buses = {}
periods = {}

for bus_name, view in context.busses.items():
    if not view.messages:
        continue
    b_cfg = bus_configs.get(bus_name)
    baud = getattr(b_cfg, "baud_rate", 500000) if b_cfg else 500000
    bus_nodes = [{"name": n, "is_external": False} for n in sorted(view.nodes)]
    msgs = []
    seen_ids = set()
    seen_names = set()
    for msg in view.messages:
        if msg.final_id in seen_ids or msg.name in seen_names:
            continue
        seen_ids.add(msg.final_id)
        seen_names.add(msg.name)
        period_ms = getattr(msg, "period", 0)
        nom_period = int(period_ms) if period_ms and int(period_ms) > 0 else None
        periods[msg.name] = nom_period
        prio = getattr(msg, "priority", 3)
        prio = max(0, min(5, int(prio))) if prio is not None else 3
        dlc = max(1, min(8, int(msg.get_dlc(context.custom_types))))
        sender = view.sender_map.get(msg.name, "UNKNOWN")
        sigs = []
        for sig in sorted(msg.signals, key=lambda x: x.bit_offset):
            choices = None
            if getattr(sig, "choices", None):
                choices = {str(i): str(c) for i, c in enumerate(sig.choices) if str(c).strip()}
            elif sig.datatype in context.custom_types and "choices" in context.custom_types[sig.datatype]:
                choices = {str(i): str(c) for i, c in enumerate(context.custom_types[sig.datatype]["choices"]) if str(c).strip()}
            elif sig.datatype == "bool" or (sig.length == 1 and not sig.is_signed):
                choices = {"0": "OFF", "1": "ON"}
            is_float = sig.datatype in ("float", "double")
            raw_t = "float32" if is_float else ("signed" if sig.is_signed else "unsigned")
            scale = float(sig.scale) if sig.scale else 1.0
            offset = float(sig.offset) if sig.offset else 0.0
            byte_order = getattr(sig, "byte_order", "little_endian") or "little_endian"
            start_bit = int(sig.bit_offset)
            bit_len = int(sig.length)
            if byte_order == "big_endian" and (start_bit % 8 == 0) and bit_len >= 8:
                start_bit += 7
            sigs.append({
                "signal_name": sig.name,
                "description": sig.desc or "",
                "data_type": sig.datatype,
                "raw_type": raw_t,
                "start_bit": start_bit,
                "bit_length": bit_len,
                "byte_order": byte_order,
                "scale": scale,
                "offset": offset,
                "limits": None,
                "unit": sig.unit or "",
                "choices": choices,
                "display_format": None,
            })
        if not sigs:
            continue
        msgs.append({
            "id": int(msg.final_id) & (0x1FFFFFFF if msg.is_extended else 0x7FF),
            "is_extended_id": bool(msg.is_extended),
            "message_name": msg.name,
            "transmitter": sender,
            "receivers": [],
            "length_bytes": dlc,
            "nominal_period_ms": nom_period,
            "priority": prio,
            "description": msg.desc or "",
            "signals": sigs,
        })
    if msgs:
        out_buses[bus_name] = {
            "bus_id": bus_id_map.get(bus_name, len(out_buses)),
            "baud_rate": baud,
            "nodes": bus_nodes,
            "messages": msgs,
        }

print("__JSON_START__")
print(json.dumps({"buses": out_buses, "periods": periods}))
"""
        (canpiler_dir / "_dump_superdbc.py").write_text(helper_code, encoding="utf-8")
        out = subprocess.check_output(
            [sys.executable, "_dump_superdbc.py"], cwd=str(canpiler_dir)
        ).decode()
        json_part = out.split("__JSON_START__", 1)[1].strip()
        parsed = json.loads(json_part)
        return parsed["buses"], parsed["periods"]


def build_2025_09_20_superdbc() -> dict:
    """Build SuperDBC for 2025-09-20-KMS from commit 6a1e891d (common/daq/node_configs/*.json)."""
    commit = "6a1e891d"
    type_bits = {
        "uint8_t": (8, False, False),
        "uint16_t": (16, False, False),
        "uint32_t": (32, False, False),
        "uint64_t": (64, False, False),
        "int8_t": (8, True, False),
        "int16_t": (16, True, False),
        "int32_t": (32, True, False),
        "int64_t": (64, True, False),
        "float": (32, True, True),
    }
    files = subprocess.check_output(
        ["git", "ls-tree", "-r", "--name-only", commit, "common/daq/node_configs"]
    ).decode().splitlines()

    nodes = []
    messages = []
    seen_names = set()
    seen_ids = set()
    node_idx = 0

    for f in sorted(files):
        if not f.endswith(".json"):
            continue
        cfg = json.loads(subprocess.check_output(["git", "show", f"{commit}:{f}"]).decode())
        for bus in cfg.get("busses", []):
            for node in bus.get("nodes", []):
                n_name = node["node_name"]
                if not any(n["name"] == n_name for n in nodes):
                    nodes.append({"name": n_name, "is_external": False})
                for msg_idx, msg in enumerate(node.get("tx", [])):
                    m_name = msg["msg_name"]
                    if m_name in seen_names:
                        continue
                    prio = max(1, min(5, int(msg.get("msg_priority", 3))))
                    if "msg_id_override" in msg:
                        can_id = int(msg["msg_id_override"], 0)
                    else:
                        can_id = (((prio - 1) & 0x7) << 26) | ((node_idx & 0x1F) << 21) | ((msg_idx & 0xFFF) << 9)
                    is_ext = can_id > 0x7FF or not bool(msg.get("is_standard_id", False))
                    can_id = can_id & (0x1FFFFFFF if is_ext else 0x7FF)
                    while (is_ext, can_id) in seen_ids:
                        can_id += 1
                    seen_names.add(m_name)
                    seen_ids.add((is_ext, can_id))

                    bit_pos = 0
                    sigs = []
                    for sig in msg.get("signals", []):
                        c_type = sig.get("type", "uint8_t")
                        default_len, is_signed, is_float = type_bits.get(c_type, (8, False, False))
                        bit_len = int(sig.get("length", default_len))
                        if bit_pos + bit_len > 64:
                            break
                        scale = float(sig.get("scale", 1.0))
                        if scale == 0.0:
                            scale = 1.0
                        offset = float(sig.get("offset", 0.0))
                        choices = None
                        if "choices" in sig and isinstance(sig["choices"], list):
                            choices = {str(i): str(c) for i, c in enumerate(sig["choices"]) if str(c).strip()}
                        elif bit_len == 1 and not is_signed:
                            choices = {"0": "OFF", "1": "ON"}
                        sigs.append({
                            "signal_name": sig["sig_name"],
                            "description": sig.get("sig_desc", ""),
                            "data_type": "bool" if (bit_len == 1 and not is_signed) else c_type,
                            "raw_type": "float32" if is_float else ("signed" if is_signed else "unsigned"),
                            "start_bit": bit_pos,
                            "bit_length": bit_len,
                            "byte_order": "little_endian",
                            "scale": scale,
                            "offset": offset,
                            "limits": None,
                            "unit": sig.get("unit", ""),
                            "choices": choices,
                            "display_format": None,
                        })
                        bit_pos += bit_len
                    if not sigs:
                        continue
                    dlc = max(1, min(8, (bit_pos + 7) // 8))
                    period = int(msg.get("msg_period", 0))
                    messages.append({
                        "id": can_id,
                        "is_extended_id": is_ext,
                        "message_name": m_name,
                        "transmitter": n_name,
                        "receivers": [],
                        "length_bytes": dlc,
                        "nominal_period_ms": period if period > 0 else None,
                        "priority": prio,
                        "description": msg.get("msg_desc", ""),
                        "signals": sigs,
                    })
                node_idx += 1

    return {
        "Main": {
            "bus_id": 0,
            "baud_rate": 500000,
            "nodes": nodes,
            "messages": messages,
        }
    }


def merge_dbc_with_periods(dbc_bus: dict, canpiler_bus: dict | None) -> dict:
    if not canpiler_bus:
        return dbc_bus
    period_by_name = {m["message_name"]: m["nominal_period_ms"] for m in canpiler_bus["messages"]}
    prio_by_name = {m["message_name"]: m["priority"] for m in canpiler_bus["messages"]}
    for m in dbc_bus["messages"]:
        if m["message_name"] in period_by_name and period_by_name[m["message_name"]] is not None:
            m["nominal_period_ms"] = period_by_name[m["message_name"]]
        if m["message_name"] in prio_by_name:
            m["priority"] = prio_by_name[m["message_name"]]
    return dbc_bus


def main() -> None:
    # 1. 2025-09-20
    buses_2025_09_20 = build_2025_09_20_superdbc()
    write_superdbc(SUPERDBC_DIR / "superdbc_2025_09_20.json", "per25_2025_09_20_6a1e891d", buses_2025_09_20)

    # 2. 2026-04-05 (0b66af8c + MCAN_9e40af6.dbc + VCAN_9e40af6.dbc)
    buses_0405_cp, periods_0405 = build_from_canpiler_commit("0b66af8c", "common/can_library")
    vcan_0405 = parse_dbc_file(PER26_DIR / "2026-04-05-KMS" / "VCAN_9e40af6.dbc", 0, 500000, periods_0405)
    mcan_0405 = parse_dbc_file(PER26_DIR / "2026-04-05-KMS" / "MCAN_9e40af6.dbc", 1, 500000, periods_0405)
    buses_2026_04_05 = {
        "VCAN": merge_dbc_with_periods(vcan_0405, buses_0405_cp.get("VCAN")),
        "MCAN": merge_dbc_with_periods(mcan_0405, buses_0405_cp.get("MCAN")),
    }
    write_superdbc(SUPERDBC_DIR / "superdbc_2026_04_05.json", "per26_2026_04_05_9e40af6", buses_2026_04_05)

    # 3. 2026-04-11 (422778b3 + MCAN_a66e8ea.dbc + VCAN_4f24069.dbc + CT3.dbc + CT3_2.dbc)
    buses_0412_cp, periods_0412 = build_from_canpiler_commit("422778b3", "common/can_library")
    vcan_0411 = parse_dbc_file(PER26_DIR / "2026-04-11-KMS" / "VCAN_4f24069.dbc", 0, 500000, periods_0412)
    mcan_0411 = parse_dbc_file(PER26_DIR / "2026-04-11-KMS" / "MCAN_a66e8ea.dbc", 1, 500000, periods_0412)
    tcan_ct3_lr = parse_dbc_file(PER26_DIR / "2026-04-11-KMS" / "CT3.dbc", 5, 1000000, {"LR_A": 1, "LR_B": 1, "LR_C": 1})
    tcan_ct3_lf = parse_dbc_file(PER26_DIR / "2026-04-12-KMS" / "CT3_2.dbc", 5, 1000000, {"LF_A": 1, "LF_B": 1, "LF_C": 1})
    tcan_combined = copy.deepcopy(tcan_ct3_lr)
    tcan_combined["nodes"].extend(tcan_ct3_lf["nodes"])
    tcan_combined["messages"].extend(tcan_ct3_lf["messages"])

    buses_2026_04_11 = {
        "VCAN": merge_dbc_with_periods(vcan_0411, buses_0412_cp.get("VCAN")),
        "MCAN": merge_dbc_with_periods(mcan_0411, buses_0412_cp.get("MCAN")),
        "TCAN": copy.deepcopy(tcan_combined),
    }
    write_superdbc(SUPERDBC_DIR / "superdbc_2026_04_11.json", "per26_2026_04_11_4f24069_ct3", buses_2026_04_11)

    # 4. 2026-04-12 (422778b3 + MCAN_4f24069.dbc + VCAN_4f24069.dbc + CT3 + CT3_2)
    mcan_0412 = parse_dbc_file(PER26_DIR / "2026-04-12-KMS" / "MCAN_4f24069.dbc", 1, 500000, periods_0412)
    buses_2026_04_12 = {
        "VCAN": merge_dbc_with_periods(copy.deepcopy(vcan_0411), buses_0412_cp.get("VCAN")),
        "MCAN": merge_dbc_with_periods(mcan_0412, buses_0412_cp.get("MCAN")),
        "TCAN": copy.deepcopy(tcan_combined),
    }
    write_superdbc(SUPERDBC_DIR / "superdbc_2026_04_12.json", "per26_2026_04_12_4f24069_ct3", buses_2026_04_12)

    # 5. 2026-05-03 (efb4ec9f)
    buses_0503, _ = build_from_canpiler_commit("efb4ec9f", "can_library")
    write_superdbc(SUPERDBC_DIR / "superdbc_2026_05_03.json", "per26_2026_05_03_efb4ec9f", buses_0503)

    # 6. 2026-05-10 (15b74a63)
    buses_0510, _ = build_from_canpiler_commit("15b74a63", "can_library")
    write_superdbc(SUPERDBC_DIR / "superdbc_2026_05_10.json", "per26_2026_05_10_15b74a63", buses_0510)

    # 7. 2026-06-14 (28d6bfa2)
    buses_0614, _ = build_from_canpiler_commit("28d6bfa2", "can_library")
    write_superdbc(SUPERDBC_DIR / "superdbc_2026_06_14.json", "per26_2026_06_14_28d6bfa2", buses_0614)

    # 8. 2026-09-06 (based on superdbc_per26.json with calibrated vbatt_adc Volts)
    base_0906 = json.loads((SUPERDBC_DIR / "superdbc_per26.json").read_text(encoding="utf-8"))
    buses_0906 = copy.deepcopy(base_0906["buses"])
    for b in buses_0906.values():
        for m in b["messages"]:
            if m["message_name"] == "pack_stats":
                for s in m["signals"]:
                    if s["signal_name"] == "vbatt_adc":
                        # 300.0 * (1.0 / 2.0) * (5.7 / 4.7) * (3.3 / 4095.0)
                        s["scale"] = 300.0 * 0.5 * (5.7 / 4.7) * (3.3 / 4095.0)
                        s["unit"] = "V"
                        s["description"] = "VBATT analog pack voltage reading"
    write_superdbc(SUPERDBC_DIR / "superdbc_2026_09_06.json", "per26_2026_09_06_vbatt_fixed", buses_0906)


if __name__ == "__main__":
    main()
