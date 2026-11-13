cbuffer constant0 : register(b0) {
	float fg_res_x;
	float fg_res_y;
	float time;
	float progress;
	float frame;
	float seed;
	float radius;
};
#define resolution float2(fg_res_x, fg_res_y)
Texture2D tex0 : register(t0);
SamplerState smp : register(s0);
float4 blur_v(float4 pos : SV_Position, float2 uv : TEXCOORD) : SV_Target {
	float4 s = 0;
	float wsum = 0;
	for (int i = -16; i <= 16; i++) {
		float w = exp(-(i * i) / 72.0);
		s += tex0.Sample(smp, uv + float2(0, i * radius / 16.0) / resolution) * w;
		wsum += w;
	}
	return s / wsum;
}