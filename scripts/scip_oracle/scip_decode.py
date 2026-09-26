"""Minimal, dependency-free decoder for the subset of SCIP this oracle reads.

Field numbers are from sourcegraph/scip `scip.proto`:
  Index            documents=2
  Document         relative_path=1 occurrences=2 symbols=3 language=4 position_encoding=6
  Occurrence       range=1 (packed int32) symbol=2 symbol_roles=3 enclosing_range=7
  SymbolInformation symbol=1 kind=5 display_name=6
Everything else is skipped by wire type, so newer SCIP producers still decode.
"""

from dataclasses import dataclass, field

ROLE_DEFINITION = 0x1


def _varint(buf, i):
    shift = result = 0
    while True:
        b = buf[i]
        i += 1
        result |= (b & 0x7F) << shift
        if b < 0x80:
            return result, i
        shift += 7


def _fields(buf):
    """Yield (field_number, wire_type, value) over one message's bytes."""
    i, n = 0, len(buf)
    while i < n:
        key, i = _varint(buf, i)
        fno, wt = key >> 3, key & 7
        if wt == 0:
            v, i = _varint(buf, i)
        elif wt == 2:
            ln, i = _varint(buf, i)
            v = buf[i:i + ln]
            i += ln
        elif wt == 1:
            v = buf[i:i + 8]
            i += 8
        elif wt == 5:
            v = buf[i:i + 4]
            i += 4
        else:
            raise ValueError(f"unsupported wire type {wt} at byte {i}")
        yield fno, wt, v


def _packed_int32(v):
    out, i = [], 0
    while i < len(v):
        x, i = _varint(v, i)
        out.append(x)
    return out


def _range(vals):
    """SCIP range: [startLine, startChar, endChar] or 4 ints. 0-based lines."""
    if len(vals) == 3:
        return (vals[0], vals[1], vals[0], vals[2])
    if len(vals) == 4:
        return tuple(vals)
    raise ValueError(f"malformed SCIP range {vals}")


@dataclass
class Occurrence:
    range: tuple
    symbol: str
    roles: int
    enclosing_range: tuple | None

    @property
    def is_definition(self):
        return bool(self.roles & ROLE_DEFINITION)


@dataclass
class Document:
    relative_path: str
    language: str = ""
    position_encoding: int = 0
    occurrences: list = field(default_factory=list)
    symbol_kinds: dict = field(default_factory=dict)


def _occurrence(buf):
    rng, sym, roles, enc = None, "", 0, None
    for fno, wt, v in _fields(buf):
        if fno == 1:
            rng = _range(_packed_int32(v) if wt == 2 else [v])
        elif fno == 2:
            sym = v.decode("utf-8")
        elif fno == 3:
            roles = v
        elif fno == 7 and wt == 2:
            enc = _range(_packed_int32(v))
    return Occurrence(rng, sym, roles, enc)


def _symbol_info(buf):
    sym, kind = "", 0
    for fno, _wt, v in _fields(buf):
        if fno == 1:
            sym = v.decode("utf-8")
        elif fno == 5:
            kind = v
    return sym, kind


def _document(buf):
    doc = Document("")
    for fno, _wt, v in _fields(buf):
        if fno == 1:
            doc.relative_path = v.decode("utf-8")
        elif fno == 2:
            doc.occurrences.append(_occurrence(v))
        elif fno == 3:
            sym, kind = _symbol_info(v)
            doc.symbol_kinds[sym] = kind
        elif fno == 4:
            doc.language = v.decode("utf-8")
        elif fno == 6:
            doc.position_encoding = v
    return doc


def read_index(path):
    with open(path, "rb") as f:
        buf = f.read()
    return [_document(v) for fno, _wt, v in _fields(buf) if fno == 2]
