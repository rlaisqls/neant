// Throughput on this core: eight independent chains of each operation, 5·10^7 laps timed inside the
// program, set against llvm-mca (tests/kernels/m7.py --own; docs/experiments.md, a model of this core).
#include <stdio.h>
#include <time.h>
#include <stdint.h>
static double now(void){struct timespec t;clock_gettime(CLOCK_MONOTONIC,&t);return t.tv_sec+t.tv_nsec*1e-9;}
#define N 50000000L
#define R8(op) a0=op(a0);a1=op(a1);a2=op(a2);a3=op(a3);a4=op(a4);a5=op(a5);a6=op(a6);a7=op(a7);
__attribute__((noinline)) double fadd8(double y){double a0=1,a1=2,a2=3,a3=4,a4=5,a5=6,a6=7,a7=8;
#define F(x) (x+y)
 for(long i=0;i<N;i++){R8(F)} return a0+a1+a2+a3+a4+a5+a6+a7;}
#undef F
__attribute__((noinline)) double fmul8(double y){double a0=1,a1=2,a2=3,a3=4,a4=5,a5=6,a6=7,a7=8;
#define F(x) (x*y)
 for(long i=0;i<N;i++){R8(F)} return a0+a1+a2+a3+a4+a5+a6+a7;}
#undef F
__attribute__((noinline)) double fma8(double y,double z){double a0=1,a1=2,a2=3,a3=4,a4=5,a5=6,a6=7,a7=8;
#define F(x) (y*z+x)
 for(long i=0;i<N;i++){R8(F)} return a0+a1+a2+a3+a4+a5+a6+a7;}
#undef F
__attribute__((noinline)) double fdiv8(double y){double a0=1,a1=2,a2=3,a3=4,a4=5,a5=6,a6=7,a7=8;
#define F(x) (x/y)
 for(long i=0;i<N/8;i++){R8(F)} return a0+a1+a2+a3+a4+a5+a6+a7;}
#undef F
__attribute__((noinline)) double fma_acc_chain(double y,double z){double x=0; for(long i=0;i<N;i++){ x = y*z + x; } return x;}
__attribute__((noinline)) int64_t iadd8(int64_t y){int64_t a0=1,a1=2,a2=3,a3=4,a4=5,a5=6,a6=7,a7=8;
#define F(x) (x+y)
 for(long i=0;i<N;i++){R8(F)} return a0+a1+a2+a3+a4+a5+a6+a7;}
#undef F
__attribute__((noinline)) int64_t imul8(int64_t y){int64_t a0=1,a1=2,a2=3,a3=4,a4=5,a5=6,a6=7,a7=8;
#define F(x) (x*y)
 for(long i=0;i<N;i++){R8(F)} return a0+a1+a2+a3+a4+a5+a6+a7;}
#undef F
__attribute__((noinline)) double ld8(const double *p, long n){double s0=0,s1=0,s2=0,s3=0; for(long i=0;i<N;i++){ long j=(i*8)&(n-1); s0+=p[j]; s1+=p[j+1]; s2+=p[j+2]; s3+=p[j+3]; } return s0+s1+s2+s3;}
static double buf[4096];
int main(int argc,char**argv){ double y=argc>5?2.0:1.0000000001, z=0.999999999; double r=0,t;
 t=now(); r+=fadd8(y); printf("fadd x8 %.3f ns/iter\n",(now()-t)/N*1e9);
 t=now(); r+=fmul8(y); printf("fmul x8 %.3f ns/iter\n",(now()-t)/N*1e9);
 t=now(); r+=fma8(y,z); printf("fma x8 %.3f ns/iter\n",(now()-t)/N*1e9);
 t=now(); r+=fdiv8(y); printf("fdiv x8 %.3f ns/iter\n",(now()-t)/(N/8)*1e9);
 t=now(); r+=fma_acc_chain(y,z); printf("fma addend chain %.3f ns/iter\n",(now()-t)/N*1e9);
 t=now(); r+=(double)iadd8((int64_t)(y*3)); printf("iadd x8 %.3f ns/iter\n",(now()-t)/N*1e9);
 t=now(); r+=(double)imul8((int64_t)(y*3)); printf("imul x8 %.3f ns/iter\n",(now()-t)/N*1e9);
 t=now(); r+=ld8(buf,4096); printf("4 loads+4 fadd chains %.3f ns/iter\n",(now()-t)/N*1e9);
 printf("%g\n",r);}
