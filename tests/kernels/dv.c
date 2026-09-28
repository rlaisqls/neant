// Division and square root on this core: sixteen independent chains each (throughput) and a
// square root feeding an add (latency), timed inside the program (docs/experiments.md, a model of this core).
#include <stdio.h>
#include <time.h>
#include <math.h>
static double now(void){struct timespec t;clock_gettime(CLOCK_MONOTONIC,&t);return t.tv_sec+t.tv_nsec*1e-9;}
#define N 20000000L
#define R16(op) a0=op(a0);a1=op(a1);a2=op(a2);a3=op(a3);a4=op(a4);a5=op(a5);a6=op(a6);a7=op(a7);b0=op(b0);b1=op(b1);b2=op(b2);b3=op(b3);b4=op(b4);b5=op(b5);b6=op(b6);b7=op(b7);
#define DECL double a0=1.1,a1=1.2,a2=1.3,a3=1.4,a4=1.5,a5=1.6,a6=1.7,a7=1.8,b0=2.1,b1=2.2,b2=2.3,b3=2.4,b4=2.5,b5=2.6,b6=2.7,b7=2.8;
#define SUM (a0+a1+a2+a3+a4+a5+a6+a7+b0+b1+b2+b3+b4+b5+b6+b7)
__attribute__((noinline)) double div16(double y){DECL
#define F(x) (x/y)
 for(long i=0;i<N;i++){R16(F)} return SUM;}
#undef F
__attribute__((noinline)) double sqrt16(void){DECL
#define F(x) (sqrt(x))
 for(long i=0;i<N;i++){R16(F)} return SUM;}
#undef F
__attribute__((noinline)) double sqrt_chain(double x){ for(long i=0;i<N;i++){ x = sqrt(x) + 1.0; } return x; }
int main(int c,char**v){double y=c>5?2.0:1.0000001,r=0,t;
 t=now(); r+=div16(y); printf("16 divs %.3f ns/iter = %.2f cycles each\n",(now()-t)/N*1e9,(now()-t)/N*1e9/0.257/16);
 t=now(); r+=sqrt16(); printf("16 sqrts %.3f ns/iter = %.2f cycles each\n",(now()-t)/N*1e9,(now()-t)/N*1e9/0.257/16);
 t=now(); r+=sqrt_chain(2.0); printf("sqrt+add chain %.3f ns = %.1f cycles\n",(now()-t)/N*1e9,(now()-t)/N*1e9/0.257);
 printf("%g\n",r);}
