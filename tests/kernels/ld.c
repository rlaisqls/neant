// Dispatch and integer throughput: eight independent L1 loads a lap (27 instructions) and twelve
// independent integer pairs (26), timed inside the program on each core (docs/experiments.md, a second core).
#include <stdio.h>
#include <time.h>
#include <stdint.h>
static double now(void){struct timespec t;clock_gettime(CLOCK_MONOTONIC,&t);return t.tv_sec+t.tv_nsec*1e-9;}
#define N 50000000L
static int64_t buf[1024];
// eight independent loads a lap from L1 (addresses do not depend on loads)
__attribute__((noinline)) int64_t loads8(void){ int64_t s0=0,s1=0,s2=0,s3=0,s4=0,s5=0,s6=0,s7=0; for(long i=0;i<N;i++){ long j=(i*8)&1016; s0+=buf[j];s1+=buf[j+1];s2+=buf[j+2];s3+=buf[j+3];s4+=buf[j+4];s5+=buf[j+5];s6+=buf[j+6];s7+=buf[j+7]; } return s0+s1+s2+s3+s4+s5+s6+s7; }
// twelve independent integer ops a lap, eor so the compiler keeps them
__attribute__((noinline)) int64_t alu12(int64_t y){ int64_t a0=1,a1=2,a2=3,a3=4,a4=5,a5=6,a6=7,a7=8,a8=9,a9=10,a10=11,a11=12;
 for(long i=0;i<N;i++){ a0^=y+i;a1^=y-i;a2^=y+2*i;a3^=y+3*i;a4^=y+4*i;a5^=y+5*i;a6^=y+6*i;a7^=y+7*i;a8^=y+8*i;a9^=y+9*i;a10^=y+10*i;a11^=y+11*i;
   __asm__ volatile("" : "+r"(a0),"+r"(a1),"+r"(a2),"+r"(a3),"+r"(a4),"+r"(a5),"+r"(a6),"+r"(a7),"+r"(a8),"+r"(a9),"+r"(a10),"+r"(a11)); }
 return a0^a1^a2^a3^a4^a5^a6^a7^a8^a9^a10^a11; }
int main(int c,char**v){ for(int i=0;i<1024;i++) buf[i]=i; double t; int64_t r=0;
 t=now(); r+=loads8(); double tl=(now()-t)/N*1e9;
 t=now(); r+=alu12(c); double ta=(now()-t)/N*1e9;
 printf("8 loads + adds: %.3f ns/lap   12 xor+add pairs: %.3f ns/lap   %ld\n", tl, ta, (long)r); }
