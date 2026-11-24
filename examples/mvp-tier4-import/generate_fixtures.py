"""Regenerate the Tier 4 fixtures with Python 3 (standard library only).

Run with --check to compare the committed files byte for byte instead.
STEP uses shared analytic B-Rep edges and explicit AP214 assembly placement;
no faceting, exporter timestamps or nondeterministic ZIP metadata are used.
"""

import argparse
from collections import defaultdict
from hashlib import sha256
from io import BytesIO
from pathlib import Path
from zipfile import ZIP_STORED, ZipFile, ZipInfo


ROOT = Path(__file__).resolve().parent
ZERO = (0, 0, 0)
X = (1, 0, 0)
Y = (0, 1, 0)
Z = (0, 0, 1)


def vector(values):
    return '(' + ','.join(str(float(value)) for value in values) + ')'


def logical(sense):
    return '.T.' if sense else '.F.'


class Step:
    def __init__(self, inch=False):
        self.entities = []
        app = self.add("APPLICATION_CONTEXT('core data for automotive mechanical design processes')")
        self.add(f"APPLICATION_PROTOCOL_DEFINITION('international standard','automotive_design',2000,{app})")
        self.product_context = self.add(f"PRODUCT_CONTEXT('',{app},'mechanical')")
        self.definition_context = self.add(f"PRODUCT_DEFINITION_CONTEXT('part definition',{app},'design')")
        length = self.add('(LENGTH_UNIT() NAMED_UNIT(*) SI_UNIT(.MILLI.,.METRE.))')
        if inch:
            dimensions = self.add('DIMENSIONAL_EXPONENTS(1.,0.,0.,0.,0.,0.,0.)')
            measure = self.add(f'LENGTH_MEASURE_WITH_UNIT(LENGTH_MEASURE(25.4),{length})')
            length = self.add(f"(CONVERSION_BASED_UNIT('INCH',{measure}) LENGTH_UNIT() NAMED_UNIT({dimensions}))")
        angle = self.add('(NAMED_UNIT(*) PLANE_ANGLE_UNIT() SI_UNIT($,.RADIAN.))')
        solid_angle = self.add('(NAMED_UNIT(*) SI_UNIT($,.STERADIAN.) SOLID_ANGLE_UNIT())')
        uncertainty = self.add(f"UNCERTAINTY_MEASURE_WITH_UNIT(LENGTH_MEASURE(1.E-07),{length},'distance_accuracy_value','')")
        self.context = self.add(
            f'(GEOMETRIC_REPRESENTATION_CONTEXT(3) GLOBAL_UNCERTAINTY_ASSIGNED_CONTEXT(({uncertainty})) '
            f"GLOBAL_UNIT_ASSIGNED_CONTEXT(({length},{angle},{solid_angle})) REPRESENTATION_CONTEXT('','3D'))"
        )
        self.frame = self.placement(ZERO)

    def add(self, definition):
        ref = f'#{len(self.entities) + 1}'
        self.entities.append(f'{ref}={definition};')
        return ref

    def point(self, coords):
        return self.add(f"CARTESIAN_POINT('',{vector(coords)})")

    def direction(self, coords):
        return self.add(f"DIRECTION('',{vector(coords)})")

    def placement(self, center, normal=Z, radial=X):
        return self.add(f"AXIS2_PLACEMENT_3D('',{self.point(center)},{self.direction(normal)},{self.direction(radial)})")

    def product(self, name, solid=None, extra_frames=()):
        product = self.add(f"PRODUCT('{name}','{name}','',({self.product_context}))")
        version = self.add(f"PRODUCT_DEFINITION_FORMATION('','',{product})")
        definition = self.add(f"PRODUCT_DEFINITION('design','',{version},{self.definition_context})")
        shape = self.add(f"PRODUCT_DEFINITION_SHAPE('','',{definition})")
        if solid:
            representation = self.add(f"ADVANCED_BREP_SHAPE_REPRESENTATION('',({self.frame},{solid}),{self.context})")
        else:
            frames = ','.join((self.frame, *extra_frames))
            representation = self.add(f"SHAPE_REPRESENTATION('',({frames}),{self.context})")
        self.add(f'SHAPE_DEFINITION_REPRESENTATION({shape},{representation})')
        self.add(f"PRODUCT_RELATED_PRODUCT_CATEGORY('part',$,({product}))")
        return definition, representation

    def occurrence(self, name, parent, child, frame):
        definition, representation = parent
        child_definition, child_representation = child
        usage = self.add(f"NEXT_ASSEMBLY_USAGE_OCCURRENCE('{name}','{name}','',{definition},{child_definition},$)")
        usage_shape = self.add(f"PRODUCT_DEFINITION_SHAPE('','',{usage})")
        transform = self.add(f"ITEM_DEFINED_TRANSFORMATION('','',{self.frame},{frame})")
        relation = self.add(
            f"(REPRESENTATION_RELATIONSHIP('','',{child_representation},{representation}) "
            f'REPRESENTATION_RELATIONSHIP_WITH_TRANSFORMATION({transform}) SHAPE_REPRESENTATION_RELATIONSHIP())'
        )
        self.add(f'CONTEXT_DEPENDENT_SHAPE_REPRESENTATION({relation},{usage_shape})')

    def encode(self, filename):
        return ('ISO-10303-21;\nHEADER;\n'
                "FILE_DESCRIPTION(('Tier 4 import fixture'),'2;1');\n"
                f"FILE_NAME('{filename}','2026-01-01T00:00:00',(''),(''),'Fixture generator','','');\n"
                "FILE_SCHEMA(('AUTOMOTIVE_DESIGN { 1 0 10303 214 1 1 1 1 }'));\nENDSEC;\nDATA;\n"
                + '\n'.join(self.entities) + '\nENDSEC;\nEND-ISO-10303-21;\n').encode('ascii')


def reversed_loop(edges):
    return [(edge, not sense) for edge, sense in reversed(edges)]


class Solid:
    def __init__(self, step):
        self.step = step
        self.vertices = {}
        self.edge_ends = {}
        self.edge_uses = defaultdict(list)
        self.lines = {}
        self.faces = []

    def vertex(self, coords):
        if coords not in self.vertices:
            self.vertices[coords] = self.step.add(f"VERTEX_POINT('',{self.step.point(coords)})")
        return self.vertices[coords]

    def edge(self, start, end, curve):
        edge = self.step.add(f"EDGE_CURVE('',{self.vertex(start)},{self.vertex(end)},{curve},.T.)")
        self.edge_ends[edge] = (start, end)
        return edge

    def line(self, start, end):
        if (end, start) in self.lines:
            return self.lines[end, start], False
        if (start, end) not in self.lines:
            delta = tuple(b - a for a, b in zip(start, end))
            magnitude = sum(value * value for value in delta) ** 0.5
            direction = self.step.direction(tuple(value / magnitude for value in delta))
            unit_vector = self.step.add(f"VECTOR('',{direction},1.)")
            curve = self.step.add(f"LINE('',{self.step.point(start)},{unit_vector})")
            self.lines[start, end] = self.edge(start, end, curve)
        return self.lines[start, end], True

    def face(self, surface, loops, surface_sense=True):
        bounds = []
        for i, edges in enumerate(loops):
            ends = [self.edge_ends[edge][::1 if sense else -1] for edge, sense in edges]
            if any(end != ends[(j + 1) % len(ends)][0] for j, (_, end) in enumerate(ends)):
                raise ValueError('Disconnected face loop')
            oriented = []
            for edge, sense in edges:
                self.edge_uses[edge].append(sense)
                oriented.append(self.step.add(f"ORIENTED_EDGE('',*,*,{edge},{logical(sense)})"))
            loop = self.step.add(f"EDGE_LOOP('',({','.join(oriented)}))")
            kind = 'FACE_OUTER_BOUND' if i == 0 else 'FACE_BOUND'
            bounds.append(self.step.add(f"{kind}('',{loop},.T.)"))
        self.faces.append(self.step.add(f"ADVANCED_FACE('',({','.join(bounds)}),{surface},{logical(surface_sense)})"))

    def plane(self, origin, loops, normal):
        radial = X if normal[0] == 0 else Y
        plane = self.step.add(f"PLANE('',{self.step.placement(origin, normal, radial)})")
        self.face(plane, loops)

    def box(self, low, high, top_inner=()):
        x0, y0, z0 = low
        x1, y1, z1 = high
        points = [(x, y, z) for z in (z0, z1) for x, y in ((x0, y0), (x1, y0), (x1, y1), (x0, y1))]
        face_specs = (
            ((0, 3, 2, 1), (0, 0, -1)), ((0, 1, 5, 4), (0, -1, 0)),
            ((1, 2, 6, 5), X), ((2, 3, 7, 6), Y), ((3, 0, 4, 7), (-1, 0, 0)),
            ((4, 5, 6, 7), Z),
        )
        for indices, normal in face_specs:
            ring = [points[i] for i in indices]
            outer = [self.line(a, b) for a, b in zip(ring, ring[1:] + ring[:1])]
            loops = [outer, *top_inner] if normal == Z else [outer]
            self.plane(ring[0], loops, normal)

    def circle(self, center, radius):
        # Two true semicircles avoid a degenerate start==end edge and a seam curve.
        curve = self.step.add(f"CIRCLE('',{self.step.placement(center)},{float(radius)})")
        x, y, z = center
        plus = (x + radius, y, z)
        minus = (x - radius, y, z)
        return [(self.edge(plus, minus, curve), True), (self.edge(minus, plus, curve), True)]

    def cylinder(self, low_center, high_center, radius, outward):
        low = self.circle(low_center, radius)
        high = self.circle(high_center, radius)
        for (lo, _), (hi, _), radial in zip(low, high, (X, (-1, 0, 0))):
            # Each half uses its own frame so its U range is [0, pi].
            frame = self.step.placement(low_center, radial=radial)
            surface = self.step.add(f"CYLINDRICAL_SURFACE('',{frame},{float(radius)})")
            lo_start, lo_end = self.edge_ends[lo]
            hi_start, hi_end = self.edge_ends[hi]
            loop = [(lo, True), self.line(lo_end, hi_end), (hi, False), self.line(hi_start, lo_start)]
            self.face(surface, [loop if outward else reversed_loop(loop)], outward)
        return low, high

    def finish(self, name):
        if self.edge_ends.keys() != self.edge_uses.keys() or any(sorted(uses) != [False, True] for uses in self.edge_uses.values()):
            raise ValueError('Closed shell requires each edge exactly twice in opposite directions')
        shell = self.step.add(f"CLOSED_SHELL('',({','.join(self.faces)}))")
        return self.step.add(f"MANIFOLD_SOLID_BREP('{name}',{shell})")


def nema17():
    # 42.3 × 42.3 × 40 mm body; top mounting datum at z=0.
    # Four nominal M3 holes (plain Ø3 cylinders, threads not modeled), depth 4.5,
    # 31 × 31 square pitch, bolt circle 31√2. Centered Ø22 × 2 mm pilot.
    # Bounds: [-21.15,-21.15,-40] .. [21.15,21.15,2].
    # Volume: 42.3²×40 + π×(11²×2 − 4×1.5²×4.5) mm³.
    step = Step()
    solid = Solid(step)
    low, high = solid.cylinder(ZERO, (0, 0, 2), 11, True)
    solid.plane((0, 0, 2), [high], Z)
    inner = [reversed_loop(low)]
    for x, y in ((-15.5, -15.5), (15.5, -15.5), (15.5, 15.5), (-15.5, 15.5)):
        bottom = (x, y, -4.5)
        low, high = solid.cylinder(bottom, (x, y, 0), 1.5, False)
        solid.plane(bottom, [low], Z)
        inner.append(reversed_loop(high))
    solid.box((-21.15, -21.15, -40), (21.15, 21.15, 0), inner)
    name = 'NEMA17_Body'
    step.product(name, solid.finish(name))
    return step.encode('nema17_mm.step')


def collar():
    # 1 in OD, 0.5 in ID, 0.25 in height; bounds in mm:
    # [-12.7,-12.7,0] .. [12.7,12.7,6.35]. Volume: π×(12.7²−6.35²)×6.35 mm³.
    step = Step(inch=True)
    solid = Solid(step)
    upper = (0, 0, 0.25)
    outer_low, outer_high = solid.cylinder(ZERO, upper, 0.5, True)
    inner_low, inner_high = solid.cylinder(ZERO, upper, 0.25, False)
    solid.plane(ZERO, [reversed_loop(outer_low), inner_low], (0, 0, -1))
    solid.plane(upper, [outer_high, reversed_loop(inner_high)], Z)
    step.product('Collar', solid.finish('Collar'))
    return step.encode('collar_inch.step')


def offset_assembly():
    # A is untransformed, [0,0,0] .. [4,3,2]. B maps (x,y,z) to
    # (10-y,20+x,5+z): world bounds [5,20,5] .. [10,22,8], not the local box.
    # World assembly bounds [0,0,0] .. [10,22,8], volume 54 mm³.
    step = Step()
    shifted = step.placement((10, 20, 5), radial=Y)
    parent = step.product('OffsetAssembly', extra_frames=(shifted,))
    for name, size, frame in (
        ('BaseBlock', (4, 3, 2), step.frame), ('OffsetBlock', (2, 5, 3), shifted),
    ):
        solid = Solid(step)
        solid.box(ZERO, size)
        child = step.product(name, solid.finish(name))
        step.occurrence(f'{name}_1', parent, child, frame)
    return step.encode('two_part_offset_asm.step')


def custom_3mf():
    # Tetrahedron with outward normals, 1 × 2 × 3 cm legs. Volume 1000 mm³.
    # Nonstandard OPC target; no 3D/3dmodel.model and no unit override in KCL.
    files = {
        '[Content_Types].xml': '''<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="model" ContentType="application/vnd.ms-package.3dmanufacturing-3dmodel+xml"/>
</Types>
''',
        '_rels/.rels': '''<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rel1" Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel" Target="/3D/parts/main.model"/>
</Relationships>
''',
        '3D/parts/main.model': '''<?xml version="1.0" encoding="UTF-8"?>
<model unit="centimeter" xml:lang="en-US" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02">
  <resources>
    <object id="1" type="model" name="Tetrahedron">
      <mesh>
        <vertices>
          <vertex x="0" y="0" z="0"/>
          <vertex x="1" y="0" z="0"/>
          <vertex x="0" y="2" z="0"/>
          <vertex x="0" y="0" z="3"/>
        </vertices>
        <triangles>
          <triangle v1="0" v2="2" v3="1"/>
          <triangle v1="0" v2="1" v3="3"/>
          <triangle v1="0" v2="3" v3="2"/>
          <triangle v1="1" v2="2" v3="3"/>
        </triangles>
      </mesh>
    </object>
  </resources>
  <build><item objectid="1"/></build>
</model>
''',
    }
    output = BytesIO()
    with ZipFile(output, 'w', compression=ZIP_STORED) as package:
        for name, content in files.items():
            member = ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
            member.create_system = 3
            member.external_attr = 0o100644 << 16
            package.writestr(member, content.encode('ascii'))
    return output.getvalue()


# One outward-facing triangle with three unpaired edges; no enclosed volume.
OPEN_STL = b'''solid open_shell
  facet normal 0 0 1
    outer loop
      vertex 0 0 0
      vertex 10 0 0
      vertex 0 10 0
    endloop
  endfacet
endsolid open_shell
'''


def example(name, asset, data, operation, extra='', comment=''):
    return (f'{comment}@unit(length = "mm", angle = "deg", force = "N")\n'
            f'part {name}() -> Solid {{\n'
            f'    return import::{operation}(\n'
            f'        path = "assets/{asset}",\n'
            f'        sha256 = "{sha256(data).hexdigest()}"{extra}\n'
            '    )\n}\n').encode('ascii')


def fixtures():
    specs = (
        ('nema17_mm.step', nema17(), 'Nema17Import', 'step', ''),
        ('collar_inch.step', collar(), 'InchCollarImport', 'step', ''),
        ('two_part_offset_asm.step', offset_assembly(), 'OffsetAssemblyImport', 'step', ''),
        ('custom_rel.3mf', custom_3mf(), 'CustomRelationshipImport', 'mesh', ''),
        ('open_shell.stl', OPEN_STL, 'OpenShellImport', 'mesh', '// Negative example: expected E505.\n'),
    )
    for asset, data, name, operation, comment in specs:
        yield ROOT / 'assets' / asset, data
        yield ROOT / (Path(asset).stem + '.kcl'), example(name, asset, data, operation, comment=comment)
    yield ROOT / 'open_shell_preview.kcl', example(
        'OpenShellPreview', 'open_shell.stl', OPEN_STL, 'mesh', ',\n        allow_open_shell = true',
        '// Preview only: expected E505 warning; not a solid for Boolean operations.\n',
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    options = parser.parse_args()
    mismatches = []
    for path, data in fixtures():
        if options.check:
            if not path.is_file() or path.read_bytes() != data:
                mismatches.append(path.relative_to(ROOT).as_posix())
        else:
            path.parent.mkdir(exist_ok=True)
            path.write_bytes(data)
    if mismatches:
        parser.exit(1, 'Missing or outdated fixture(s): ' + ', '.join(mismatches) + '\n')


if __name__ == '__main__':
    main()
