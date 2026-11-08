// CorralGlass.metal
// The Corral's signature liquid-glass material: a metaball field of agent blobs
// joined by thin liquid bridges, refracting the desktop behind it.
//
// Used through SwiftUI's .layerEffect. Corral requires macOS 26: the layer it is applied to
// is the system Liquid Glass backdrop, and this pass carves it into drops and bridges and adds
// the rim, highlights and caustics (docs/07 §4). Everything outside the shape returns
// transparent, plus a soft shadow.
//
// Status: SPIKE S5 prototype. Not yet compiled on a Mac. The maths is mirrored in
// preview/corral-shape-preview.html and was checked with a numpy render.
//
// Blob payload: float array, 4 floats per blob: cx, cy, radius, attach (0…1).
// The first `ringCount` blobs are bridged i → i+1 (mod ringCount), or just 0 → 1 when ringCount == 2.

#include <metal_stdlib>
#include <SwiftUI/SwiftUI_Metal.h>
using namespace metal;

// ---------- signed distance helpers ----------

static float sdCircle(float2 p, float2 c, float r) { return length(p - c) - r; }

static float sdCapsule(float2 p, float2 a, float2 b, float r) {
    float2 pa = p - a, ba = b - a;
    float h = clamp(dot(pa, ba) / max(dot(ba, ba), 1e-4), 0.0, 1.0);
    return length(pa - ba * h) - r;
}

// Polynomial smooth-min: blends shapes with a liquid "neck" of size k.
static float smin(float a, float b, float k) {
    float h = clamp(0.5 + 0.5 * (b - a) / k, 0.0, 1.0);
    return mix(b, a, h) - k * h * (1.0 - h);
}

static float corralField(float2 p,
                         device const float *blobs, int floatCount,
                         float ringCount, float bridgeRatio, float neck)
{
    int n = floatCount / 4;
    int ring = int(ringCount);
    float d = 1e5;

    // Blobs
    for (int i = 0; i < n; i++) {
        float2 c = float2(blobs[i*4], blobs[i*4+1]);
        float  r = blobs[i*4+2];
        d = smin(d, sdCircle(p, c, r), neck);
    }

    // Bridges along the ring (attach thins a bridge to nothing as a blob pinches off)
    int bridgeCount = (ring == 2) ? 1 : (ring >= 3 ? ring : 0);
    for (int i = 0; i < bridgeCount; i++) {
        int j = (i + 1) % ring;
        float2 a = float2(blobs[i*4], blobs[i*4+1]);
        float2 b = float2(blobs[j*4], blobs[j*4+1]);
        float  r = min(blobs[i*4+2], blobs[j*4+2]) * bridgeRatio;
        float  att = min(blobs[i*4+3], blobs[j*4+3]);
        if (att <= 0.001) continue;
        d = smin(d, sdCapsule(p, a, b, r * att), neck * att);
    }
    return d;
}

static float hash21(float2 p) {
    p = fract(p * float2(123.34, 456.21));
    p += dot(p, p + 45.32);
    return fract(p.x * p.y);
}

// ---------- the material ----------
//
// size        : view size in points
// time        : seconds, drives a very slow caustic drift (0 when Reduce Motion is on)
// blobs       : payload (see header)
// ringCount   : number of ring members at the front of the payload
// tint        : glass edge tint (brand: cool blue-grey)
// params      : x = refraction strength (pt), y = bevel width (pt), z = frost (0…1), w = neck (smin k)
[[ stitchable ]] half4 corralGlass(float2 position,
                                   SwiftUI::Layer layer,
                                   float2 size,
                                   float time,
                                   device const float *blobs, int floatCount,
                                   float ringCount,
                                   half4 tint,
                                   float4 params)
{
    const float refractPx = params.x;
    const float bevel     = params.y;
    const float frost     = params.z;
    const float neck      = params.w;
    const float bridgeRatio = 0.22;   // keep in sync with CorralMetrics.bridgeWidthRatio

    float d = corralField(position, blobs, floatCount, ringCount, bridgeRatio, neck);

    // ---- outside: soft contact shadow, offset down-right like the reference render ----
    if (d > 0.5) {
        float ds = corralField(position - float2(3.0, 5.0), blobs, floatCount, ringCount, bridgeRatio, neck);
        float shadow = (1.0 - smoothstep(0.0, 14.0, ds)) * 0.16;
        return half4(0.0, 0.02, 0.06, 1.0) * half(shadow);   // premultiplied
    }

    // ---- surface normal from the field gradient ----
    const float e = 1.0;
    float dx = corralField(position + float2(e, 0), blobs, floatCount, ringCount, bridgeRatio, neck)
             - corralField(position - float2(e, 0), blobs, floatCount, ringCount, bridgeRatio, neck);
    float dy = corralField(position + float2(0, e), blobs, floatCount, ringCount, bridgeRatio, neck)
             - corralField(position - float2(0, e), blobs, floatCount, ringCount, bridgeRatio, neck);
    float2 g = normalize(float2(dx, dy) + 1e-5);

    // Height profile: flat top, rounded bevel near the rim (like a drop of water on glass).
    float inside = clamp(-d / bevel, 0.0, 1.0);       // 0 at rim → 1 past the bevel
    float slope  = 1.0 - inside;                       // steep at rim
    float3 n = normalize(float3(g * slope * 1.6, 1.0));

    // ---- refraction: bend the backdrop along the normal, strongest at the rim ----
    float2 offset = -g * refractPx * slope * slope;
    float2 uv = position + offset;

    // Light frost: 5-tap cross blur, wider near the rim.
    float f = 1.0 + frost * 3.0 * slope;
    half4 col = layer.sample(uv) * 0.4h
              + layer.sample(uv + float2( f, 0)) * 0.15h
              + layer.sample(uv + float2(-f, 0)) * 0.15h
              + layer.sample(uv + float2(0,  f)) * 0.15h
              + layer.sample(uv + float2(0, -f)) * 0.15h;

    // Slight chromatic split at the very edge.
    half r = layer.sample(uv - g * 0.8 * slope).r;
    half b = layer.sample(uv + g * 0.8 * slope).b;
    col.r = mix(col.r, r, half(slope));
    col.b = mix(col.b, b, half(slope));

    // ---- lighting ----
    float3 L = normalize(float3(-0.45, -0.75, 0.9));    // key light from top-left
    float3 V = float3(0, 0, 1);
    float3 H = normalize(L + V);
    float spec  = pow(max(dot(n, H), 0.0), 60.0) * 0.55;
    float fres  = pow(1.0 - max(dot(n, V), 0.0), 3.0);
    float rim   = smoothstep(2.5, 0.0, -d) * 0.35;      // thin bright meniscus line

    // Inner shadow on the far side of the bevel gives the "thick water" look.
    float inner = smoothstep(0.0, 1.0, slope) * max(dot(g, normalize(float2(0.5, 0.8))), 0.0) * 0.18;

    // Slow caustic shimmer (static when time is held at 0).
    float caustic = 0.5 + 0.5 * sin(position.x * 0.045 + time * 0.35) * sin(position.y * 0.05 - time * 0.25);
    caustic *= inside * 0.04;

    // Tiny air bubbles: sparse, fixed per pixel cell, like the reference.
    float2 cell = floor(position / 18.0);
    float2 local = fract(position / 18.0) - 0.5;
    float bubble = 0.0;
    if (hash21(cell) > 0.93 && inside > 0.6) {
        float bd = length(local - (float2(hash21(cell + 7.1), hash21(cell + 3.3)) - 0.5) * 0.6);
        bubble = (1.0 - smoothstep(0.03, 0.07, bd)) * 0.35;
    }

    half3 rgb = col.rgb;
    rgb = mix(rgb, tint.rgb, half(fres * 0.55 + 0.06));     // edge tint
    rgb += half3(spec + rim + caustic);
    rgb -= half3(inner + bubble * 0.6);

    // Anti-aliased coverage at the rim.
    float a = clamp(0.5 - d, 0.0, 1.0);
    return half4(saturate(rgb), 1.0h) * half(a);
}
