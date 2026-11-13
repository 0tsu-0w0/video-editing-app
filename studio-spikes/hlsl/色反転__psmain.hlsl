cbuffer constant0 : register(b0) {
	float fg_res_x;
	float fg_res_y;
	float time;
	float progress;
	float frame;
	float seed;
	float amount;
};
#define resolution float2(fg_res_x, fg_res_y)
Texture2D tex0 : register(t0);
SamplerState smp : register(s0);
float4 psmain(float4 pos : SV_Position, float2 uv : TEXCOORD) : SV_Target {
	float4 c = tex0.Sample(smp, uv);
	float a = max(c.a, 1e-5);
	float3 rgb = c.rgb / a;
	rgb = lerp(rgb, 1.0 - rgb, amount);
	return float4(rgb * c.a, c.a);
}