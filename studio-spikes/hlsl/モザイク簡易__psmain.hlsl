cbuffer constant0 : register(b0) {
	float fg_res_x;
	float fg_res_y;
	float time;
	float progress;
	float frame;
	float seed;
	float size;
};
#define resolution float2(fg_res_x, fg_res_y)
Texture2D tex0 : register(t0);
SamplerState smp : register(s0);
float4 psmain(float4 pos : SV_Position, float2 uv : TEXCOORD) : SV_Target {
	float2 cell = max(size, 1.0);
	float2 p = (floor(pos.xy / cell) + 0.5) * cell;
	return tex0.Sample(smp, clamp(p, 0.5, resolution - 0.5) / resolution);
}