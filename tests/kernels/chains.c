// Dependency chains, one a function, 10^8 laps each timed inside the program: the latency of an
// f64 add, multiply, multiply-add and divide and an i64 multiply-add on this core, to set against
// llvm-mca's cycles (tests/kernels/mca.py; docs/experiments.md, M7's first probe). gcc -O2.
#include <stdio.h>
#include <time.h>
#include <stdint.h>
static double now(void){struct timespec t;clock_gettime(CLOCK_MONOTONIC,&t);return t.tv_sec+t.tv_nsec*1e-9;}
#define N 100000000L
__attribute__((noinline)) double fadd_chain(double x, double y){ for(long i=0;i<N;i++){ x = x + y; } return x; }
__attribute__((noinline)) double fmul_chain(double x, double y){ for(long i=0;i<N;i++){ x = x * y; } return x; }
__attribute__((noinline)) double fma_chain(double x, double y, double z){ for(long i=0;i<N;i++){ x = x * y + z; } return x; }
__attribute__((noinline)) double fdiv_chain(double x, double y){ for(long i=0;i<N;i++){ x = x / y; } return x; }
__attribute__((noinline)) int64_t imul_chain(int64_t x, int64_t y){ for(long i=0;i<N;i++){ x = x * y + 1; } return x; }
__attribute__((noinline)) double fadd_indep(double *a, double y){ double s0=0,s1=0,s2=0,s3=0; for(long i=0;i<N;i++){ s0+=y; s1+=y; s2+=y; s3+=y; } return s0+s1+s2+s3; }
int main(int argc, char**argv){
  double y = argc > 5 ? 2.0 : 1.0000000001, r=0; double t;
  t=now(); r+=fadd_chain(1.0,y); printf("fadd_chain %.3f ns\n",(now()-t)/N*1e9);
  t=now(); r+=fmul_chain(1.0,y); printf("fmul_chain %.3f ns\n",(now()-t)/N*1e9);
  t=now(); r+=fma_chain(1.0,y,0.5); printf("fma_chain %.3f ns\n",(now()-t)/N*1e9);
  t=now(); r+=fdiv_chain(1.0,y); printf("fdiv_chain %.3f ns\n",(now()-t)/N*1e9);
  t=now(); r+=(double)imul_chain(3,y>1.5?5:7); printf("imul_chain %.3f ns\n",(now()-t)/N*1e9);
  t=now(); r+=fadd_indep(0,y); printf("fadd_indep4 %.3f ns\n",(now()-t)/N*1e9);
  printf("%g\n", r);
}
