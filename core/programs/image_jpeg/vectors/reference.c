#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <jpeglib.h>

static void decode(FILE *input, FILE *output, unsigned scale)
{
    struct jpeg_decompress_struct c;
    struct jpeg_error_mgr error;
    c.err = jpeg_std_error(&error);
    jpeg_create_decompress(&c);
    jpeg_stdio_src(&c, input);
    jpeg_read_header(&c, TRUE);
    c.out_color_space = JCS_RGB;
    c.scale_num = scale;
    c.scale_denom = 8;
    jpeg_start_decompress(&c);
    unsigned dimensions[] = { c.output_width, c.output_height };
    for (unsigned axis = 0; axis < 2; ++axis) {
        for (int shift = 24; shift >= 0; shift -= 8) {
            if (fputc((dimensions[axis] >> shift) & 255, output) == EOF)
                exit(1);
        }
    }
    size_t stride = 3 * c.output_width;
    JSAMPROW row = malloc(stride);
    if (!row)
        exit(1);
    while (c.output_scanline < c.output_height) {
        if (jpeg_read_scanlines(&c, &row, 1) != 1 || fwrite(row, 1, stride, output) != stride)
            exit(1);
    }
    jpeg_finish_decompress(&c);
    jpeg_destroy_decompress(&c);
    free(row);
}

static void encode(FILE *input, FILE *output, unsigned mode, unsigned width, unsigned height)
{
    struct jpeg_compress_struct c;
    struct jpeg_error_mgr error;
    c.err = jpeg_std_error(&error);
    jpeg_create_compress(&c);
    jpeg_stdio_dest(&c, output);
    c.image_width = width;
    c.image_height = height;
    c.input_components = 3;
    c.in_color_space = JCS_RGB;
    jpeg_set_defaults(&c);
    jpeg_set_quality(&c, 95, TRUE);
    if (!(mode & 1)) {
        c.comp_info[0].h_samp_factor = 1;
        c.comp_info[0].v_samp_factor = 1;
    }
    if (mode & 2)
        jpeg_simple_progression(&c);
    jpeg_start_compress(&c, TRUE);
    size_t stride = 3 * width;
    JSAMPROW row = malloc(stride);
    if (!row)
        exit(1);
    while (c.next_scanline < c.image_height) {
        if (fread(row, 1, stride, input) != stride || jpeg_write_scanlines(&c, &row, 1) != 1)
            exit(1);
    }
    if (fgetc(input) != EOF || ferror(input))
        exit(1);
    jpeg_finish_compress(&c);
    jpeg_destroy_compress(&c);
    free(row);
}

int main(int argc, char **argv)
{
    if (argc != 5 && argc != 7)
        return 2;
    FILE *input = fopen(argv[2], "rb");
    FILE *output = fopen(argv[3], "wb");
    if (!input || !output)
        return 1;
    unsigned mode = (unsigned)strtoul(argv[4], NULL, 10);
    if (argc == 5 && strcmp(argv[1], "decode") == 0) {
        decode(input, output, mode);
    } else if (argc == 7 && strcmp(argv[1], "encode") == 0) {
        encode(input, output, mode, (unsigned)strtoul(argv[5], NULL, 10),
               (unsigned)strtoul(argv[6], NULL, 10));
    } else {
        return 2;
    }
    return fclose(output) != 0 || fclose(input) != 0;
}
