cbuffer constant0 : register(b0) {
	float fg_res_x;
	float fg_res_y;
	float time;
	float progress;
	float frame;
	float seed;
	float noise_intensity;
	float color_shift;
	float jitter;
	float scanlines;
	float tint_r;
	float tint_g;
	float tint_b;
	float grain;
};
#define resolution float2(fg_res_x, fg_res_y)
#define tint float3(tint_r, tint_g, tint_b)
Texture2D tex0 : register(t0);
Texture2D tex1 : register(t1);
SamplerState smp : register(s0);
float fg_hash(float2 p) {

	return tex1.SampleLevel(smp, p / 256.0, 0).r;
}
float4 psmain(float4 pos : SV_Position, float2 uv : TEXCOORD) : SV_Target {
	float t = floor(time * 30.0);
	float cell = lerp(1.0, 4.0, grain);
	float line_n = fg_hash(float2(floor(pos.y / 2.0) + seed, t)) - 0.5;
	float2 ofs = float2(line_n * jitter, 0.0) / resolution;
	float2 sh = float2(color_shift, 0.0) / resolution;
	float4 cr = tex0.Sample(smp, uv + ofs + sh);
	float4 cg = tex0.Sample(smp, uv + ofs);
	float4 cb = tex0.Sample(smp, uv + ofs - sh);
	float a = max(max(cr.a, cg.a), cb.a);
	float3 rgb = float3(cr.r, cg.g, cb.b);
	float n = fg_hash(floor(pos.xy / cell) + float2(t * 17.0, t * 31.0 + seed)) - 0.5;
	rgb += n * noise_intensity * a;
	if (scanlines > 0.5) {
		rgb *= 0.85 + 0.15 * sin(pos.y * 3.14159265);
	}
	rgb *= tint;
	rgb = clamp(rgb, 0.0, a);
	return float4(rgb, a);
}