// The C half of `main.lilac`: each function takes and returns structs by
// value, and some call back into Lilac's exports, directly and through
// function pointers, so both sides of every call are the C compiler's.
#include <stdint.h>

typedef struct { float x, y; } Vec2;
typedef struct { float x, y, z; } Vec3;
typedef struct { uint8_t r, g, b, a; } Color;
typedef struct { uint8_t a, b, c; } Rgb;
typedef struct { double d; uint32_t i; } Mixed;
typedef struct { uint64_t a, b, c; } Big;
typedef struct { uint8_t tag; union { uint32_t i; float f; } payload; } Num;
typedef struct { Vec2 position; Color tint; } Sprite;

Vec2 vec2_add(Vec2 a, Vec2 b) { return (Vec2){a.x + b.x, a.y + b.y}; }

Vec3 vec3_scale(Vec3 v, float s) { return (Vec3){v.x * s, v.y * s, v.z * s}; }

Color color_invert(Color c) {
    return (Color){(uint8_t)(255 - c.r), (uint8_t)(255 - c.g), (uint8_t)(255 - c.b), c.a};
}

Rgb rgb_rotate(Rgb c) { return (Rgb){c.c, c.a, c.b}; }

Mixed mixed_bump(Mixed m) {
    m.d += 0.5;
    m.i += 1;
    return m;
}

Big big_sum(Big a, Big b, Big c, Big d, uint32_t scale) {
    return (Big){(a.a + b.a + c.a + d.a) * scale, (a.b + b.b + c.b + d.b) * scale,
                 (a.c + b.c + c.c + d.c) * scale};
}

Num num_double(Num n) {
    Num r = n;
    if (n.tag == 0) {
        r.payload.i = n.payload.i * 2;
    } else {
        r.payload.f = n.payload.f * 2;
    }
    return r;
}

Sprite sprite_move(Sprite s, Vec2 by) {
    s.position = vec2_add(s.position, by);
    s.tint.a = 128;
    return s;
}

// Lilac's exports, called by name.
Rgb lilac_rgb_swap(Rgb);
Big lilac_big_twice(Big, uint32_t);

uint64_t call_lilac(void) {
    Rgb r = lilac_rgb_swap((Rgb){1, 2, 3});
    Big b = lilac_big_twice((Big){1, 2, 3}, 2);
    return r.a * 100u + r.b * 10u + r.c + (b.a + b.b + b.c) * 1000u;
}

// Lilac functions passed as C function pointers.
Vec3 apply_vec3(Vec3 (*f)(Vec3, float), Vec3 v, float s) { return f(v, s); }

Mixed apply_mixed(Mixed (*f)(Mixed), Mixed m) { return f(f(m)); }
