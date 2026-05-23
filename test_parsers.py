import os
from parsers import load_model

def run_tests():
    print("=== Testing Geometry Parsers and Mesh Analyzers ===")
    
    test_cases = [
        ('models/ascii_cube.stl', 12, 64000.0),
        ('models/binary_cylinder.stl', 128, 35118.0),  # Volume: pi * r^2 * h = 3.14159 * 15^2 * 50 ~ 35,343. Approximate due to 32 slices
        ('models/pyramid_mesh.3mf', 6, 33333.3),      # Volume: 1/3 * base_area * height = 1/3 * 50*50 * 40 = 33,333.33
        ('models/hierarchical_assembly.3mf', 48, 13500.0), # Volume: 4 cubes * (15^3) = 4 * 3375 = 13500. Add coordinate checks.
        ('models/cube.obj', 12, 8000.0)                # Wavefront OBJ Cube: 20x20x20 = 8000.0 volume
    ]
    
    for filepath, expected_triangles, expected_volume in test_cases:
        if not os.path.exists(filepath):
            print(f"Error: Test file not found: {filepath}")
            continue
            
        print(f"\nLoading: {filepath}")
        try:
            model = load_model(filepath)
            
            # Print parsed details
            print(f"  Format:   {model.format}")
            print(f"  Vertices: {model.num_vertices}")
            print(f"  Triangles: {model.num_triangles} (Expected: {expected_triangles})")
            
            sz = model.bounding_box_size
            print(f"  Bounds:   {sz[0]:.2f} x {sz[1]:.2f} x {sz[2]:.2f} mm")
            
            # volume comparison
            err_pct = abs(model.volume - expected_volume) / expected_volume * 100.0 if expected_volume > 0 else 0.0
            print(f"  Volume:   {model.volume:.2f} mm³ (Expected: {expected_volume:.2f} mm³, Error: {err_pct:.2f}%)")
            
            # Assertions
            assert model.num_triangles == expected_triangles, f"Mismatch triangle count for {filepath}"
            assert err_pct < 5.0, f"Volume calculation error too high for {filepath}: {err_pct:.2f}%"
            
            print(f"  [PASS] {filepath} verified successfully.")
            
        except Exception as e:
            print(f"  [FAIL] Failed loading or validating {filepath}: {e}")
            import traceback
            traceback.print_exc()

    # Specialized test case for G-Code paths
    gcode_path = 'models/spiral_path.gcode'
    if os.path.exists(gcode_path):
        print(f"\nLoading G-Code: {gcode_path}")
        try:
            model = load_model(gcode_path)
            print(f"  Format:   {model.format}")
            print(f"  Vertices: {model.num_vertices}")
            print(f"  Lines:     {model.num_lines} (Expected: 41)")
            
            sz = model.bounding_box_size
            print(f"  Bounds:   {sz[0]:.2f} x {sz[1]:.2f} x {sz[2]:.2f} mm")
            
            # Assertions
            assert model.num_triangles == 0, "G-Code should have 0 triangles"
            assert model.num_lines == 41, f"Expected 41 line segments, got {model.num_lines}"
            assert model.num_vertices == 82, f"Expected 82 vertices (2 per line segment), got {model.num_vertices}"
            
            print(f"  [PASS] {gcode_path} verified successfully.")
        except Exception as e:
            print(f"  [FAIL] Failed loading or validating {gcode_path}: {e}")
            import traceback
            traceback.print_exc()

if __name__ == '__main__':
    run_tests()
