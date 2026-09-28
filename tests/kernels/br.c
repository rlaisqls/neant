// A loop exit the predictor misses: the same inner loop with trips random in 0..4 and constant 2,
// the difference an entry (docs/experiments.md, a model of this core).
#include <stdio.h>
#include <time.h>
#include <stdlib.h>
static double now(void){struct timespec t;clock_gettime(CLOCK_MONOTONIC,&t);return t.tv_sec+t.tv_nsec*1e-9;}
#define N 20000000L
static unsigned char trips[N];
__attribute__((noinline)) long run(const unsigned char *tr){ long s=0; for(long i=0;i<N;i++){ for(int j=0;j<tr[i];j++){ s += j ^ i; } } return s; }
int main(){ srand(1); long total=0; for(long i=0;i<N;i++){ trips[i]=rand()%5; total+=trips[i]; }
 double t=now(); long r=run(trips); double tv=now()-t;
 for(long i=0;i<N;i++) trips[i]=2;
 t=now(); r+=run(trips); double tc=now()-t;
 printf("varying 0..4 (mean 2): %.3f ns/entry, constant 2: %.3f ns/entry, difference %.2f ns = %.1f cycles an entry\n", tv/N*1e9, tc/N*1e9, (tv-tc)/N*1e9, (tv-tc)/N*1e9/0.257);
 printf("%ld %ld\n", r, total); }
