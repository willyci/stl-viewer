import struct

def generate_bmp(filename):
    width, height = 256, 256
    pixel_data_offset = 54
    row_size = (width * 3 + 3) & ~3  # Pad row size to multiples of 4 bytes
    pixel_data_size = row_size * height
    file_size = pixel_data_offset + pixel_data_size

    # BMP Header & DIB Header
    header = struct.pack('<2sIHHI', b'BM', file_size, 0, 0, pixel_data_offset)
    dib = struct.pack('<IIIHHIIiiII', 40, width, height, 1, 24, 0, pixel_data_size, 2835, 2835, 0, 0)

    pixels = bytearray()
    for y in range(height):
        row = bytearray()
        for x in range(width):
            # Create a premium cyan and blue geometric pattern
            r = int((x / width) * 40)
            g = int((y / height) * 240)
            b = int(((x + y) / (width + height)) * 255)
            row.extend([b, g, r])
        while len(row) < row_size:
            row.append(0)
        pixels.extend(row)

    with open(filename, 'wb') as f:
        f.write(header)
        f.write(dib)
        f.write(pixels)

if __name__ == '__main__':
    generate_bmp('c:/dev/stl_viewer/models/test_pattern.bmp')
    print("Successfully generated test_pattern.bmp!")
