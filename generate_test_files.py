import os
import struct
import zipfile
import math
import numpy as np

def generate_cube_stl_ascii(filepath):
    """Generate a clean ASCII STL Cube (size 40x40x40)."""
    # 8 vertices of a centered cube
    v = np.array([
        [-20.0, -20.0, -20.0],
        [ 20.0, -20.0, -20.0],
        [ 20.0,  20.0, -20.0],
        [-20.0,  20.0, -20.0],
        [-20.0, -20.0,  20.0],
        [ 20.0, -20.0,  20.0],
        [ 20.0,  20.0,  20.0],
        [-20.0,  20.0,  20.0]
    ])
    
    # 12 triangles (2 per face)
    faces = [
        # Bottom (Z = -20)
        (0, 2, 1, [0, 0, -1]), (0, 3, 2, [0, 0, -1]),
        # Top (Z = 20)
        (4, 5, 6, [0, 0, 1]), (4, 6, 7, [0, 0, 1]),
        # Front (Y = -20)
        (0, 1, 5, [0, -1, 0]), (0, 5, 4, [0, -1, 0]),
        # Back (Y = 20)
        (2, 3, 7, [0, 1, 0]), (2, 7, 6, [0, 1, 0]),
        # Left (X = -20)
        (0, 4, 7, [-1, 0, 0]), (0, 7, 3, [-1, 0, 0]),
        # Right (X = 20)
        (1, 2, 6, [1, 0, 0]), (1, 6, 5, [1, 0, 0])
    ]
    
    with open(filepath, 'w') as f:
        f.write("solid ascii_cube\n")
        for f1, f2, f3, n in faces:
            f.write(f"  facet normal {n[0]:.6f} {n[1]:.6f} {n[2]:.6f}\n")
            f.write("    outer loop\n")
            f.write(f"      vertex {v[f1][0]:.6f} {v[f1][1]:.6f} {v[f1][2]:.6f}\n")
            f.write(f"      vertex {v[f2][0]:.6f} {v[f2][1]:.6f} {v[f2][2]:.6f}\n")
            f.write(f"      vertex {v[f3][0]:.6f} {v[f3][1]:.6f} {v[f3][2]:.6f}\n")
            f.write("    endloop\n")
            f.write("  endfacet\n")
        f.write("endsolid ascii_cube\n")


def generate_cylinder_stl_binary(filepath):
    """Generate a clean Binary STL Cylinder (radius 15, height 50, 32 slices)."""
    radius = 15.0
    height = 50.0
    slices = 32
    
    vertices = []
    triangles = []
    normals = []
    
    # Bottom center
    v_bot_center = [0.0, 0.0, 0.0]
    # Top center
    v_top_center = [0.0, 0.0, height]
    
    vertices.append(v_bot_center)  # index 0
    vertices.append(v_top_center)  # index 1
    
    # Generate circle vertices
    for i in range(slices):
        angle = 2.0 * math.pi * i / slices
        x = radius * math.cos(angle)
        y = radius * math.sin(angle)
        
        # Bottom ring point
        vertices.append([x, y, 0.0])  # index 2 + 2*i
        # Top ring point
        vertices.append([x, y, height])  # index 3 + 2*i
        
    # Build faces
    for i in range(slices):
        next_i = (i + 1) % slices
        
        b_curr = 2 + 2 * i
        t_curr = 3 + 2 * i
        b_next = 2 + 2 * next_i
        t_next = 3 + 2 * next_i
        
        # Bottom cap triangle (pointing down)
        triangles.append((0, b_next, b_curr))
        normals.append([0.0, 0.0, -1.0])
        
        # Top cap triangle (pointing up)
        triangles.append((1, t_curr, t_next))
        normals.append([0.0, 0.0, 1.0])
        
        # Side rectangle facet 1
        triangles.append((b_curr, t_next, t_curr))
        # Compute normal
        angle_avg = 2.0 * math.pi * (i + 0.5) / slices
        nx = math.cos(angle_avg)
        ny = math.sin(angle_avg)
        normals.append([nx, ny, 0.0])
        
        # Side rectangle facet 2
        triangles.append((b_curr, b_next, t_next))
        normals.append([nx, ny, 0.0])
        
    num_triangles = len(triangles)
    
    # Write Binary STL
    with open(filepath, 'wb') as f:
        # 80-byte header
        header = b"Binary STL Cylinder".ljust(80, b'\0')
        f.write(header)
        
        # 4-byte count
        f.write(struct.pack('<I', num_triangles))
        
        # Triangles
        for idx, (f1, f2, f3) in enumerate(triangles):
            n = normals[idx]
            v1 = vertices[f1]
            v2 = vertices[f2]
            v3 = vertices[f3]
            
            # Format: 3 floats normal, 9 floats vertices, 2 bytes attribute
            data = struct.pack(
                '<ffffffffffffH',
                n[0], n[1], n[2],
                v1[0], v1[1], v1[2],
                v2[0], v2[1], v2[2],
                v3[0], v3[1], v3[2],
                0
            )
            f.write(data)


def generate_pyramid_3mf(filepath):
    """Generate a clean 3MF file containing a single Pyramid mesh."""
    # 3D Model XML representation
    model_xml = """<?xml version="1.0" encoding="utf-8"?>
<model xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02" unit="millimeter">
  <resources>
    <object id="1" type="model" name="Pyramid">
      <mesh>
        <vertices>
          <vertex x="-25.000" y="-25.000" z="0.000" />
          <vertex x="25.000" y="-25.000" z="0.000" />
          <vertex x="25.000" y="25.000" z="0.000" />
          <vertex x="-25.000" y="25.000" z="0.000" />
          <vertex x="0.000" y="0.000" z="40.000" />
        </vertices>
        <triangles>
          <triangle v1="0" v2="2" v3="1" />
          <triangle v1="0" v2="3" v3="2" />
          <triangle v1="0" v2="1" v3="4" />
          <triangle v1="1" v2="2" v3="4" />
          <triangle v1="2" v2="3" v3="4" />
          <triangle v1="3" v2="0" v3="4" />
        </triangles>
      </mesh>
    </object>
  </resources>
  <build>
    <item objectid="1" />
  </build>
</model>
"""
    
    # 3MF is a standard ZIP container containing 3D/3dmodel.model
    with zipfile.ZipFile(filepath, 'w', zipfile.ZIP_DEFLATED) as z:
        z.writestr('3D/3dmodel.model', model_xml)
        
        # Add basic dummy content types to be a valid zip compliant
        content_types = """<?xml version="1.0" encoding="utf-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="model" ContentType="application/vnd.ms-package.3dmanufacturing-3dmodel+xml"/>
</Types>
"""
        z.writestr('[Content_Types].xml', content_types)


def generate_assembly_3mf(filepath):
    """Generate a hierarchical 3MF Assembly file to test components and transformations."""
    model_xml = """<?xml version="1.0" encoding="utf-8"?>
<model xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02" unit="millimeter">
  <resources>
    <!-- Object 1: Small Cube (size 15x15x15) -->
    <object id="1" type="model" name="SmallCube">
      <mesh>
        <vertices>
          <vertex x="0.0" y="0.0" z="0.0" />
          <vertex x="15.0" y="0.0" z="0.0" />
          <vertex x="15.0" y="15.0" z="0.0" />
          <vertex x="0.0" y="15.0" z="0.0" />
          <vertex x="0.0" y="0.0" z="15.0" />
          <vertex x="15.0" y="0.0" z="15.0" />
          <vertex x="15.0" y="15.0" z="15.0" />
          <vertex x="0.0" y="15.0" z="15.0" />
        </vertices>
        <triangles>
          <triangle v1="0" v2="2" v3="1" />
          <triangle v1="0" v2="3" v3="2" />
          <triangle v1="4" v2="5" v3="6" />
          <triangle v1="4" v2="6" v3="7" />
          <triangle v1="0" v2="1" v3="5" />
          <triangle v1="0" v2="5" v3="4" />
          <triangle v1="1" v2="2" v3="6" />
          <triangle v1="1" v2="6" v3="5" />
          <triangle v1="2" v2="3" v3="7" />
          <triangle v1="2" v2="7" v3="6" />
          <triangle v1="3" v2="0" v3="4" />
          <triangle v1="3" v2="4" v3="7" />
        </triangles>
      </mesh>
    </object>
    
    <!-- Object 2: Star assembly of 4 cubes at different locations and rotations -->
    <object id="2" type="model" name="CubeAssembly">
      <components>
        <!-- Center-Left cube -->
        <component objectid="1" transform="1 0 0 0 1 0 0 0 1 -30 0 0" />
        <!-- Center-Right cube -->
        <component objectid="1" transform="1 0 0 0 1 0 0 0 1 30 0 0" />
        <!-- Center-Top cube rotated 45 deg around Z -->
        <component objectid="1" transform="0.707 -0.707 0 0.707 0.707 0 0 0 1 0 30 0" />
        <!-- Center-Bottom cube rotated -45 deg around Z -->
        <component objectid="1" transform="0.707 0.707 0 -0.707 0.707 0 0 0 1 0 -30 0" />
      </components>
    </object>
  </resources>
  <build>
    <item objectid="2" />
  </build>
</model>
"""
    with zipfile.ZipFile(filepath, 'w', zipfile.ZIP_DEFLATED) as z:
        z.writestr('3D/3dmodel.model', model_xml)
        
        content_types = """<?xml version="1.0" encoding="utf-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="model" ContentType="application/vnd.ms-package.3dmanufacturing-3dmodel+xml"/>
</Types>
"""
        z.writestr('[Content_Types].xml', content_types)


def generate_cube_obj(filepath):
    """Generate a Wavefront OBJ Cube (size 20x20x20)."""
    with open(filepath, 'w') as f:
        f.write("# Wavefront OBJ Cube\n")
        f.write("v -10.0 -10.0 -10.0\n")
        f.write("v 10.0 -10.0 -10.0\n")
        f.write("v 10.0 10.0 -10.0\n")
        f.write("v -10.0 10.0 -10.0\n")
        f.write("v -10.0 -10.0 10.0\n")
        f.write("v 10.0 -10.0 10.0\n")
        f.write("v 10.0 10.0 10.0\n")
        f.write("v -10.0 10.0 10.0\n")
        
        # 6 quad faces to test fan triangulation (6 quads -> 12 triangles)
        f.write("f 1 2 3 4\n")
        f.write("f 5 8 7 6\n")
        f.write("f 1 5 6 2\n")
        f.write("f 2 6 7 3\n")
        f.write("f 3 7 8 4\n")
        f.write("f 5 1 4 8\n")


def generate_spiral_gcode(filepath):
    """Generate a sample G-Code representing a 3D printing spiral path."""
    with open(filepath, 'w') as f:
        f.write("; Mock G-Code Spiral\n")
        f.write("G90 ; Absolute positioning\n")
        f.write("G1 Z0.2 F1200\n")
        f.write("G1 X0.0 Y0.0 E0.0\n")
        
        x, y, z = 0.0, 0.0, 0.2
        e = 0.0
        step = 1.5
        for i in range(1, 41):
            dist = i * step
            if i % 4 == 1:
                x = dist
            elif i % 4 == 2:
                y = dist
            elif i % 4 == 3:
                x = -dist
            else:
                y = -dist
                z += 0.2  # increment layer height
            e += 0.8
            f.write(f"G1 X{x:.3f} Y{y:.3f} Z{z:.3f} E{e:.4f} F1500\n")


def main():
    print("Generating validation test models...")
    os.makedirs('models', exist_ok=True)
    
    generate_cube_stl_ascii('models/ascii_cube.stl')
    print("  -> Created models/ascii_cube.stl (ASCII STL)")
    
    generate_cylinder_stl_binary('models/binary_cylinder.stl')
    print("  -> Created models/binary_cylinder.stl (Binary STL)")
    
    generate_pyramid_3mf('models/pyramid_mesh.3mf')
    print("  -> Created models/pyramid_mesh.3mf (Single 3MF)")
    
    generate_assembly_3mf('models/hierarchical_assembly.3mf')
    print("  -> Created models/hierarchical_assembly.3mf (Assembly 3MF with nested transforms)")
    
    generate_cube_obj('models/cube.obj')
    print("  -> Created models/cube.obj (Wavefront OBJ Cube)")
    
    generate_spiral_gcode('models/spiral_path.gcode')
    print("  -> Created models/spiral_path.gcode (Spiral G-Code Toolpath)")
    
    print("\nGeneration completed successfully! All files written to workspace/models/ directory.")

if __name__ == '__main__':
    main()
