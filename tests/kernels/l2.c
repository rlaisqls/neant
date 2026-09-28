// Load latency on this core by working set: a random cycle of lines, one dependent load a step,
// 16 KB to 2 MB (docs/experiments.md, a model of this core).
#include <stdio.h>
#include <stdlib.h>
#include <time.h>
static double now(void){struct timespec t;clock_gettime(CLOCK_MONOTONIC,&t);return t.tv_sec+t.tv_nsec*1e-9;}
int main(){ for (long kb=16; kb<=2048; kb*=2){ long n=kb*1024/64; long *p=malloc(n*64); long *idx=malloc(n*sizeof(long));
 for(long i=0;i<n;i++) idx[i]=i; srand(7); for(long i=n-1;i>0;i--){ long j=rand()%(i+1); long t=idx[i]; idx[i]=idx[j]; idx[j]=t; }
 for(long i=0;i<n;i++) p[idx[i]*8]=idx[(i+1)%n]*8;
 long x=0, steps=20000000; double t=now(); for(long s=0;s<steps;s++) x=p[x]; double d=now()-t;
 printf("%5ld KB: %.2f ns = %.1f cycles a dependent load\n", kb, d/steps*1e9, d/steps*1e9/0.257); free(p); free(idx); if(x==-1) puts(""); } }
