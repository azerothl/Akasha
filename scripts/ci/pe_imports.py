"""Read load-time DLL imports of PE32/PE32+ files using the standard library."""
from pathlib import Path
import struct


def imports(path):
    data = Path(path).read_bytes()
    def u16(offset):
        return struct.unpack_from("<H", data, offset)[0]
    def u32(offset):
        return struct.unpack_from("<I", data, offset)[0]
    if data[:2] != b"MZ":
        raise ValueError("not a PE executable")
    pe = u32(0x3C)
    if data[pe:pe + 4] != b"PE\0\0":
        raise ValueError("invalid PE signature")
    optional = pe + 24
    magic = u16(optional)
    if magic not in (0x10B, 0x20B):
        raise ValueError("unsupported optional header")
    directory_offset = 112 if magic == 0x20B else 96
    if u32(optional + directory_offset - 4) < 2:
        return []
    table = optional + u16(pe + 20)
    sections = []
    for index in range(u16(pe + 6)):
        section = table + index * 40
        virtual_size, address, raw_size, raw_offset = struct.unpack_from("<IIII", data, section + 8)
        sections.append((address, max(virtual_size, raw_size), raw_offset, raw_size))
    def offset(rva):
        for address, size, raw, raw_size in sections:
            if address <= rva < address + size:
                relative = rva - address
                if relative >= raw_size:
                    raise ValueError("RVA has no raw bytes")
                return raw + relative
        raise ValueError(f"RVA outside sections: {rva}")
    import_rva, size = struct.unpack_from("<II", data, optional + directory_offset + 8)
    if not import_rva:
        return []
    start = offset(import_rva)
    names = []
    for descriptor in range(start, min(start + size, len(data)) - 19, 20):
        if data[descriptor:descriptor + 20] == bytes(20):
            return sorted(set(names))
        name = offset(u32(descriptor + 12))
        end = data.find(b"\0", name, min(name + 256, len(data)))
        if end < 0:
            raise ValueError("unterminated DLL name")
        names.append(data[name:end].decode("ascii").lower())
    raise ValueError("unterminated import directory")
