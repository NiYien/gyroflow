// SPDX-License-Identifier: GPL-3.0-or-later
// OpenCL vector semantics for the shared stabilization equations in CUDA C++.
typedef unsigned char uchar; typedef unsigned int uint;
#define M_PI_F 3.14159265358979323846f
#define __global
#define __private
#define __constant __device__ __constant__
#define __kernel extern "C" __global__
#define get_global_id(d) ((d)==0 ? blockIdx.x*blockDim.x+threadIdx.x : blockIdx.y*blockDim.y+threadIdx.y)
struct __align__(8) gf_float2 { float x,y; __device__ gf_float2() {} __device__ constexpr gf_float2(float a):x(a), y(a){} __device__ constexpr gf_float2(float a0, float a1):x(a0), y(a1){} };
#define float2 gf_float2
__device__ float2 operator+(float2 a,float2 b){return float2(a.x+b.x, a.y+b.y);}
__device__ float2& operator+=(float2& a,float2 b){a=a+b; return a;}
__device__ float2 operator-(float2 a,float2 b){return float2(a.x-b.x, a.y-b.y);}
__device__ float2& operator-=(float2& a,float2 b){a=a-b; return a;}
__device__ float2 operator*(float2 a,float2 b){return float2(a.x*b.x, a.y*b.y);}
__device__ float2& operator*=(float2& a,float2 b){a=a*b; return a;}
__device__ float2 operator/(float2 a,float2 b){return float2(a.x/b.x, a.y/b.y);}
__device__ float2& operator/=(float2& a,float2 b){a=a/b; return a;}
__device__ float2 operator-(float2 a){return float2(-a.x, -a.y);}
__device__ float2 round(float2 a){return float2(roundf(a.x), roundf(a.y));}
__device__ float2 fabs(float2 a){return float2(fabsf(a.x), fabsf(a.y));}
__device__ float2 floor(float2 a){return float2(floorf(a.x), floorf(a.y));}
__device__ float2 ceil(float2 a){return float2(ceilf(a.x), ceilf(a.y));}
__device__ float2 sqrt(float2 a){return float2(sqrtf(a.x), sqrtf(a.y));}
__device__ float2 sin(float2 a){return float2(sinf(a.x), sinf(a.y));}
__device__ float2 cos(float2 a){return float2(cosf(a.x), cosf(a.y));}
__device__ float2 min(float2 a,float2 b){return float2(fminf(a.x,b.x), fminf(a.y,b.y));}
__device__ float2 max(float2 a,float2 b){return float2(fmaxf(a.x,b.x), fmaxf(a.y,b.y));}
__device__ float2 fmin(float2 a,float2 b){return float2(fminf(a.x,b.x), fminf(a.y,b.y));}
__device__ float2 fmax(float2 a,float2 b){return float2(fmaxf(a.x,b.x), fmaxf(a.y,b.y));}
__device__ float dot(float2 a,float2 b){return a.x*b.x+a.y*b.y;}
__device__ float length(float2 a){return sqrtf(dot(a,a));}
__device__ float2 normalize(float2 a){return a/float2(length(a));}
__device__ float2 clamp(float2 a,float lo,float hi){return max(float2(lo), min(float2(hi),a));}
struct __align__(4) gf_float3 { float x,y,z; __device__ gf_float3() {} __device__ constexpr gf_float3(float a):x(a), y(a), z(a){} __device__ constexpr gf_float3(float a0, float a1, float a2):x(a0), y(a1), z(a2){} };
#define float3 gf_float3
__device__ float3 operator+(float3 a,float3 b){return float3(a.x+b.x, a.y+b.y, a.z+b.z);}
__device__ float3& operator+=(float3& a,float3 b){a=a+b; return a;}
__device__ float3 operator-(float3 a,float3 b){return float3(a.x-b.x, a.y-b.y, a.z-b.z);}
__device__ float3& operator-=(float3& a,float3 b){a=a-b; return a;}
__device__ float3 operator*(float3 a,float3 b){return float3(a.x*b.x, a.y*b.y, a.z*b.z);}
__device__ float3& operator*=(float3& a,float3 b){a=a*b; return a;}
__device__ float3 operator/(float3 a,float3 b){return float3(a.x/b.x, a.y/b.y, a.z/b.z);}
__device__ float3& operator/=(float3& a,float3 b){a=a/b; return a;}
__device__ float3 operator-(float3 a){return float3(-a.x, -a.y, -a.z);}
__device__ float3 round(float3 a){return float3(roundf(a.x), roundf(a.y), roundf(a.z));}
__device__ float3 fabs(float3 a){return float3(fabsf(a.x), fabsf(a.y), fabsf(a.z));}
__device__ float3 floor(float3 a){return float3(floorf(a.x), floorf(a.y), floorf(a.z));}
__device__ float3 ceil(float3 a){return float3(ceilf(a.x), ceilf(a.y), ceilf(a.z));}
__device__ float3 sqrt(float3 a){return float3(sqrtf(a.x), sqrtf(a.y), sqrtf(a.z));}
__device__ float3 sin(float3 a){return float3(sinf(a.x), sinf(a.y), sinf(a.z));}
__device__ float3 cos(float3 a){return float3(cosf(a.x), cosf(a.y), cosf(a.z));}
__device__ float3 min(float3 a,float3 b){return float3(fminf(a.x,b.x), fminf(a.y,b.y), fminf(a.z,b.z));}
__device__ float3 max(float3 a,float3 b){return float3(fmaxf(a.x,b.x), fmaxf(a.y,b.y), fmaxf(a.z,b.z));}
__device__ float3 fmin(float3 a,float3 b){return float3(fminf(a.x,b.x), fminf(a.y,b.y), fminf(a.z,b.z));}
__device__ float3 fmax(float3 a,float3 b){return float3(fmaxf(a.x,b.x), fmaxf(a.y,b.y), fmaxf(a.z,b.z));}
__device__ float dot(float3 a,float3 b){return a.x*b.x+a.y*b.y+a.z*b.z;}
__device__ float length(float3 a){return sqrtf(dot(a,a));}
__device__ float3 normalize(float3 a){return a/float3(length(a));}
__device__ float3 clamp(float3 a,float lo,float hi){return max(float3(lo), min(float3(hi),a));}
struct __align__(16) gf_float4 { float x,y,z,w; __device__ gf_float4() {} __device__ constexpr gf_float4(float a):x(a), y(a), z(a), w(a){} __device__ constexpr gf_float4(float a0, float a1, float a2, float a3):x(a0), y(a1), z(a2), w(a3){} };
#define float4 gf_float4
__device__ float4 operator+(float4 a,float4 b){return float4(a.x+b.x, a.y+b.y, a.z+b.z, a.w+b.w);}
__device__ float4& operator+=(float4& a,float4 b){a=a+b; return a;}
__device__ float4 operator-(float4 a,float4 b){return float4(a.x-b.x, a.y-b.y, a.z-b.z, a.w-b.w);}
__device__ float4& operator-=(float4& a,float4 b){a=a-b; return a;}
__device__ float4 operator*(float4 a,float4 b){return float4(a.x*b.x, a.y*b.y, a.z*b.z, a.w*b.w);}
__device__ float4& operator*=(float4& a,float4 b){a=a*b; return a;}
__device__ float4 operator/(float4 a,float4 b){return float4(a.x/b.x, a.y/b.y, a.z/b.z, a.w/b.w);}
__device__ float4& operator/=(float4& a,float4 b){a=a/b; return a;}
__device__ float4 operator-(float4 a){return float4(-a.x, -a.y, -a.z, -a.w);}
__device__ float4 round(float4 a){return float4(roundf(a.x), roundf(a.y), roundf(a.z), roundf(a.w));}
__device__ float4 fabs(float4 a){return float4(fabsf(a.x), fabsf(a.y), fabsf(a.z), fabsf(a.w));}
__device__ float4 floor(float4 a){return float4(floorf(a.x), floorf(a.y), floorf(a.z), floorf(a.w));}
__device__ float4 ceil(float4 a){return float4(ceilf(a.x), ceilf(a.y), ceilf(a.z), ceilf(a.w));}
__device__ float4 sqrt(float4 a){return float4(sqrtf(a.x), sqrtf(a.y), sqrtf(a.z), sqrtf(a.w));}
__device__ float4 sin(float4 a){return float4(sinf(a.x), sinf(a.y), sinf(a.z), sinf(a.w));}
__device__ float4 cos(float4 a){return float4(cosf(a.x), cosf(a.y), cosf(a.z), cosf(a.w));}
__device__ float4 min(float4 a,float4 b){return float4(fminf(a.x,b.x), fminf(a.y,b.y), fminf(a.z,b.z), fminf(a.w,b.w));}
__device__ float4 max(float4 a,float4 b){return float4(fmaxf(a.x,b.x), fmaxf(a.y,b.y), fmaxf(a.z,b.z), fmaxf(a.w,b.w));}
__device__ float4 fmin(float4 a,float4 b){return float4(fminf(a.x,b.x), fminf(a.y,b.y), fminf(a.z,b.z), fminf(a.w,b.w));}
__device__ float4 fmax(float4 a,float4 b){return float4(fmaxf(a.x,b.x), fmaxf(a.y,b.y), fmaxf(a.z,b.z), fmaxf(a.w,b.w));}
__device__ float dot(float4 a,float4 b){return a.x*b.x+a.y*b.y+a.z*b.z+a.w*b.w;}
__device__ float length(float4 a){return sqrtf(dot(a,a));}
__device__ float4 normalize(float4 a){return a/float4(length(a));}
__device__ float4 clamp(float4 a,float lo,float hi){return max(float4(lo), min(float4(hi),a));}
__device__ float clamp(float x,float lo,float hi){return fmaxf(lo,fminf(hi,x));}
__device__ int convert_int_sat_rtz(float x){return (int)fminf(2147483520.0f,fmaxf(-2147483648.0f,x));}
__device__ int convert_int_sat_rte(float x){return __float2int_rn(fminf(2147483520.0f,fmaxf(-2147483648.0f,x)));}
__device__ float4 convert_float4(float4 x){return x;}
__device__ float2 convert_float2(float2 x){return x;}
__device__ float mix(float a,float b,float t){return a*(1-t)+b*t;}
__device__ float2 mix(float2 a,float2 b,float t){return a*(1-t)+b*t;}

__device__ int4 gf_make_int4(int a,int b,int c,int d){int4 v;v.x=a;v.y=b;v.z=c;v.w=d;return v;}
// Fuse only color accumulation. Coordinate arithmetic keeps --fmad=false so
// interpolation phase selection does not change at quantization boundaries.
__device__ gf_float4 color_mad(gf_float4 a,float b,gf_float4 c){return gf_float4(fmaf(a.x,b,c.x),fmaf(a.y,b,c.y),fmaf(a.z,b,c.z),fmaf(a.w,b,c.w));}
