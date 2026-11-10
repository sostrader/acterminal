// Camera/Params + shared helpers come from common.wgsl

struct VertexInput {
    @location(0) local_pos: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@group(1) @binding(0)
var heatmap_rg: texture_2d<u32>;

// Liquidity-heatmap color ramps, ported stop-for-stop from ACTerminal
// `bookmap.ts`'s `CMAPS` table (RGB 0..255 stops, normalized here to 0..1).

fn ramp_stops5(s0: vec3<f32>, s1: vec3<f32>, s2: vec3<f32>, s3: vec3<f32>, s4: vec3<f32>, t: f32) -> vec3<f32> {
    let tc = clamp(t, 0.0, 1.0);
    let scaled = tc * 4.0;
    let i = clamp(floor(scaled), 0.0, 3.0);
    let frac = scaled - i;
    if (i < 0.5) {
        return mix(s0, s1, frac);
    } else if (i < 1.5) {
        return mix(s1, s2, frac);
    } else if (i < 2.5) {
        return mix(s2, s3, frac);
    } else {
        return mix(s3, s4, frac);
    }
}

fn ramp_stops6(
    s0: vec3<f32>, s1: vec3<f32>, s2: vec3<f32>, s3: vec3<f32>, s4: vec3<f32>, s5: vec3<f32>, t: f32
) -> vec3<f32> {
    let tc = clamp(t, 0.0, 1.0);
    let scaled = tc * 5.0;
    let i = clamp(floor(scaled), 0.0, 4.0);
    let frac = scaled - i;
    if (i < 0.5) {
        return mix(s0, s1, frac);
    } else if (i < 1.5) {
        return mix(s1, s2, frac);
    } else if (i < 2.5) {
        return mix(s2, s3, frac);
    } else if (i < 3.5) {
        return mix(s3, s4, frac);
    } else {
        return mix(s4, s5, frac);
    }
}

// `colormap` order matches `data::chart::bookmap::Colormap::ALL`
// (0=Bookmap, 1=Mono, 2=Inferno, 3=Viridis).
fn cmap_color(colormap: u32, is_bid: bool, t: f32) -> vec3<f32> {
    if (colormap == 0u) {
        if (is_bid) {
            return ramp_stops5(
                vec3<f32>(6.0, 10.0, 16.0) / 255.0,
                vec3<f32>(6.0, 55.0, 65.0) / 255.0,
                vec3<f32>(20.0, 140.0, 135.0) / 255.0,
                vec3<f32>(125.0, 225.0, 210.0) / 255.0,
                vec3<f32>(235.0, 255.0, 250.0) / 255.0,
                t,
            );
        } else {
            return ramp_stops5(
                vec3<f32>(6.0, 10.0, 16.0) / 255.0,
                vec3<f32>(60.0, 18.0, 18.0) / 255.0,
                vec3<f32>(170.0, 40.0, 40.0) / 255.0,
                vec3<f32>(245.0, 140.0, 90.0) / 255.0,
                vec3<f32>(255.0, 235.0, 200.0) / 255.0,
                t,
            );
        }
    } else if (colormap == 1u) {
        return ramp_stops5(
            vec3<f32>(8.0, 10.0, 14.0) / 255.0,
            vec3<f32>(55.0, 60.0, 72.0) / 255.0,
            vec3<f32>(135.0, 145.0, 160.0) / 255.0,
            vec3<f32>(220.0, 225.0, 235.0) / 255.0,
            vec3<f32>(255.0, 255.0, 255.0) / 255.0,
            t,
        );
    } else if (colormap == 2u) {
        return ramp_stops6(
            vec3<f32>(0.0, 0.0, 4.0) / 255.0,
            vec3<f32>(40.0, 11.0, 84.0) / 255.0,
            vec3<f32>(120.0, 28.0, 109.0) / 255.0,
            vec3<f32>(200.0, 55.0, 85.0) / 255.0,
            vec3<f32>(252.0, 135.0, 40.0) / 255.0,
            vec3<f32>(252.0, 255.0, 164.0) / 255.0,
            t,
        );
    } else {
        return ramp_stops5(
            vec3<f32>(68.0, 1.0, 84.0) / 255.0,
            vec3<f32>(59.0, 82.0, 139.0) / 255.0,
            vec3<f32>(33.0, 144.0, 141.0) / 255.0,
            vec3<f32>(93.0, 201.0, 99.0) / 255.0,
            vec3<f32>(253.0, 231.0, 37.0) / 255.0,
            t,
        );
    }
}

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.pos = vec4<f32>(input.local_pos * 2.0, 0.0, 1.0);
    out.uv = input.local_pos + vec2<f32>(0.5, 0.5);
    return out;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let viewport = camera.b.xy;

    let screen_xy = input.uv * viewport;
    let world = screen_to_world(screen_xy);

    if (world.x > 0.0) {
        return vec4<f32>(0.0);
    }

    let col_w = max(params.grid.x, 1e-12);
    let row_h = max(params.grid.y, 1e-12);
    let steps_per = max(params.grid.z, 1.0);
    let steps_per_i = max(i32(round(steps_per)), 1);

    let tex_w_u = u32(params.heatmap_tex.x);
    let tex_h_u = u32(params.heatmap_tex.y);
    if (tex_w_u < 1u || tex_h_u < 1u) {
        return vec4<f32>(0.0);
    }

    let steps_at_y = i32(floor((-world.y) / max(row_h, 1e-12)));
    let y_bin_rel = steps_at_y / steps_per_i;
    let y_start_bin = i32(params.heatmap_map.y);
    let yi = y_bin_rel - y_start_bin;
    if (yi < 0 || u32(yi) >= tex_h_u) {
        return vec4<f32>(0.0);
    }

    let latest_bucket_rel = i32(params.heatmap_map.x);
    let render_rel = i32(floor(params.origin.x + (world.x / col_w)));

    var bucket_rel_from_latest = render_rel - latest_bucket_rel;
    bucket_rel_from_latest = min(bucket_rel_from_latest, 0);

    let oldest = -i32(tex_w_u) + 1;
    if (bucket_rel_from_latest < oldest) {
        return vec4<f32>(0.0);
    }

    let latest_x_ring = i32(u32(params.heatmap_map.w));
    let tex_w_mask = i32(u32(params.heatmap_tex.z));
    let xi = (latest_x_ring + bucket_rel_from_latest) & tex_w_mask;

    let inv_qty_scale = params.heatmap_tex.w;

    let v = textureLoad(heatmap_rg, vec2<i32>(xi, yi), 0);
    let bid_qty = f32(v.x) * inv_qty_scale;
    let ask_qty = f32(v.y) * inv_qty_scale;

    let max_depth = max(params.depth.x, 1e-12);
    let alpha_min = params.depth.y;
    let alpha_max = params.depth.z;
    let gamma = max(params.depth.w, 0.01);
    let colormap = u32(params.bid_rgb.w);

    let bid_t = pow(clamp(bid_qty / max_depth, 0.0, 1.0), gamma);
    let ask_t = pow(clamp(ask_qty / max_depth, 0.0, 1.0), gamma);

    let bid_a = select(0.0, alpha_min + bid_t * (alpha_max - alpha_min), bid_qty > 0.0);
    let ask_a = select(0.0, alpha_min + ask_t * (alpha_max - alpha_min), ask_qty > 0.0);

    let bid_color = cmap_color(colormap, true, bid_t);
    let ask_color = cmap_color(colormap, false, ask_t);

    var c = ask_color * ask_a;
    var a = ask_a;

    c = c + (1.0 - a) * bid_color * bid_a;
    a = a + (1.0 - a) * bid_a;

    let fade = fade_factor(world.x);
    return vec4<f32>(c * fade, a * fade);
}