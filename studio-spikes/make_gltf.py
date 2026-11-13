"""検証用の、骨アニメーション付き glTF を作る。
円柱(高さ2、根元と中ほどに2本の骨)。中ほどの骨が 0→60°→0 と曲がり、根元が Y 軸で回る。
分割数を変えて、軽いモデルと重いモデル(約10万三角形)を作る。"""
import json, math, struct, sys, base64

def make(path, seg, rings):
    pos, nrm, jnt, wgt, idx = [], [], [], [], []
    for r in range(rings + 1):
        y = 2.0 * r / rings
        w1 = min(max((y - 0.6) / 0.8, 0.0), 1.0)  # 0.6〜1.4 で根元→上の骨へ移る
        rad = 0.3 + 0.05 * math.sin(y * 6)
        for s in range(seg):
            a = 2 * math.pi * s / seg
            x, z = math.cos(a), math.sin(a)
            pos += [rad * x, y, rad * z]
            nrm += [x, 0.0, z]
            jnt += [0, 1, 0, 0]
            wgt += [1 - w1, w1, 0.0, 0.0]
    for r in range(rings):
        for s in range(seg):
            a = r * seg + s; b = r * seg + (s + 1) % seg
            c = a + seg; d = b + seg
            idx += [a, c, b, b, c, d]
    nv = len(pos) // 3
    ibm = [1,0,0,0, 0,1,0,0, 0,0,1,0, 0,0,0,1,   1,0,0,0, 0,1,0,0, 0,0,1,0, 0,-1,0,1]
    times = [0.0, 1.0, 2.0]
    def qz(deg):
        h = math.radians(deg) / 2; return [0, 0, math.sin(h), math.cos(h)]
    def qy(deg):
        h = math.radians(deg) / 2; return [0, math.sin(h), 0, math.cos(h)]
    rot_child = qz(0) + qz(60) + qz(0)
    rot_root = qy(0) + qy(180) + qy(360)

    chunks, views, accs = [], [], []
    def add(fmt, data, comp, typ, count, target=None, minmax=None):
        raw = struct.pack('<%d%s' % (len(data), fmt), *data)
        off = sum(len(c) for c in chunks)
        pad = (-len(raw)) % 4
        chunks.append(raw + b'\0' * pad)
        v = {"buffer": 0, "byteOffset": off, "byteLength": len(raw)}
        if target: v["target"] = target
        views.append(v)
        a = {"bufferView": len(views) - 1, "componentType": comp, "count": count, "type": typ}
        if minmax: a["min"], a["max"] = minmax
        accs.append(a)
        return len(accs) - 1
    xs, ys, zs = pos[0::3], pos[1::3], pos[2::3]
    P = add('f', pos, 5126, "VEC3", nv, 34962, ([min(xs), min(ys), min(zs)], [max(xs), max(ys), max(zs)]))
    N = add('f', nrm, 5126, "VEC3", nv, 34962)
    J = add('H', jnt, 5123, "VEC4", nv, 34962)
    Wt = add('f', wgt, 5126, "VEC4", nv, 34962)
    I = add('I', idx, 5125, "SCALAR", len(idx), 34963)
    B = add('f', ibm, 5126, "MAT4", 2)
    T = add('f', times, 5126, "SCALAR", 3, None, ([0.0], [2.0]))
    RC = add('f', rot_child, 5126, "VEC4", 3)
    RR = add('f', rot_root, 5126, "VEC4", 3)
    data = b''.join(chunks)
    doc = {
        "asset": {"version": "2.0", "generator": "studio-spike"},
        "scene": 0, "scenes": [{"nodes": [0, 1]}],
        "nodes": [
            {"name": "body", "mesh": 0, "skin": 0},
            {"name": "root", "children": [2]},
            {"name": "upper", "translation": [0, 1, 0]},
        ],
        "meshes": [{"primitives": [{"attributes": {"POSITION": P, "NORMAL": N, "JOINTS_0": J, "WEIGHTS_0": Wt},
                                     "indices": I, "material": 0}]}],
        "materials": [{"pbrMetallicRoughness": {"baseColorFactor": [0.95, 0.55, 0.65, 1.0], "metallicFactor": 0.0}}],
        "skins": [{"joints": [1, 2], "inverseBindMatrices": B}],
        "animations": [{"name": "bend", "samplers": [
            {"input": T, "output": RC, "interpolation": "LINEAR"},
            {"input": T, "output": RR, "interpolation": "LINEAR"}],
            "channels": [{"sampler": 0, "target": {"node": 2, "path": "rotation"}},
                         {"sampler": 1, "target": {"node": 1, "path": "rotation"}}]}],
        "buffers": [{"byteLength": len(data), "uri": "data:application/octet-stream;base64," + base64.b64encode(data).decode()}],
        "bufferViews": views, "accessors": accs,
    }
    json.dump(doc, open(path, 'w'))
    print(path, "頂点", nv, "三角形", len(idx) // 3)

make('assets/bend_light.gltf', 32, 24)
make('assets/bend_heavy.gltf', 256, 200)
