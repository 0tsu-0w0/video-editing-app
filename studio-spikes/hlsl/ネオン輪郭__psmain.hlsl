cbuffer constant0 : register(b0) {
	float fg_res_x;
	float fg_res_y;
	float time;
	float progress;
	float frame;
	float seed;
	float strength;
	float width;
	float glow_color_r;
	float glow_color_g;
	float glow_color_b;
	float keep_source;
	float pulse;
};
#define resolution float2(fg_res_x, fg_res_y)
#define glow_color float3(glow_color_r, glow_color_g, glow_color_b)
Texture2D tex0 : register(t0);
SamplerState smp : register(s0);
float luma(float4 c) { return dot(c.rgb, float3(0.299, 0.587, 0.114)); }
float4 psmain(float4 pos : SV_Position, float2 uv : TEXCOORD) : SV_Target {
	float2 px = width / resolution;
	float gx = 0, gy = 0;
	for (int j = -1; j <= 1; j++) {
		for (int i = -1; i <= 1; i++) {
			float4 s = tex0.Sample(smp, uv + float2(i, j) * px);
			float v = luma(s) + s.a;
			gx += v * i * (j == 0 ? 2 : 1);
			gy += v * j * (i == 0 ? 2 : 1);
		}
	}
	float edge = saturate(sqrt(gx * gx + gy * gy));
	float flick = 0.75 + 0.25 * sin(time * pulse * 6.2831853);
	float4 src = tex0.Sample(smp, uv);
	float3 glow = glow_color * edge * strength * flick;
	float a = saturate(max(src.a * keep_source, edge));
	return float4(src.rgb * keep_source + glow, a);
}