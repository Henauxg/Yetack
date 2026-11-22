// Times ggml's matrix products at the sizes of Parakeet's encoder, with 8-bit (Q8_0) and f32
// weights, on the CPU: kernel-bench-ggml <threads>. kernels/candle times the same with candle.

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>

#include "ggml.h"
#include "ggml-cpu.h"

static const int WEIGHTS[3][2] = {{4096, 1024}, {1024, 4096}, {1024, 1024}};
static const int FRAMES[4] = {7, 64, 125, 375};

static float random_normal(void) {
    float u = (rand() + 1.0f) / (RAND_MAX + 2.0f), v = (rand() + 1.0f) / (RAND_MAX + 2.0f);
    return sqrtf(-2.0f * logf(u)) * cosf(6.2831853f * v);
}

static int compare(const void *a, const void *b) {
    double x = *(const double *)a, y = *(const double *)b;
    return (x > y) - (x < y);
}

int main(int argc, char **argv) {
    int threads = argc > 1 ? atoi(argv[1]) : 4;
    ggml_time_init();
    fprintf(stderr, "avx2 %d fma %d f16c %d avx_vnni %d avx512 %d\n", ggml_cpu_has_avx2(), ggml_cpu_has_fma(),
            ggml_cpu_has_f16c(), ggml_cpu_has_avx_vnni(), ggml_cpu_has_avx512());
    // A pool that lives across runs, as transcribe.cpp keeps one: starting threads each run
    // would dominate the small products.
    struct ggml_threadpool_params pool_params = ggml_threadpool_params_default(threads);
    struct ggml_threadpool *pool = ggml_threadpool_new(&pool_params);
    enum ggml_type types[2] = {GGML_TYPE_Q8_0, GGML_TYPE_F32};
    const char *names[2] = {"q8_0", "f32"};
    for (int s = 0; s < 3; s++) {
        int n = WEIGHTS[s][0], k = WEIGHTS[s][1];
        float *source = malloc(sizeof(float) * n * k);
        for (int i = 0; i < n * k; i++) source[i] = random_normal();
        for (int f = 0; f < 4; f++) {
            int m = FRAMES[f];
            for (int t = 0; t < 2; t++) {
                size_t memory = ggml_row_size(types[t], k) * n + sizeof(float) * (size_t)(m * k + m * n)
                    + 256 * 1024 * 1024;
                struct ggml_init_params params = {memory, NULL, false};
                struct ggml_context *ctx = ggml_init(params);
                struct ggml_tensor *weight = ggml_new_tensor_2d(ctx, types[t], k, n);
                if (types[t] == GGML_TYPE_F32) {
                    memcpy(weight->data, source, sizeof(float) * n * k);
                } else {
                    ggml_quantize_chunk(types[t], source, weight->data, 0, n, k, NULL);
                }
                struct ggml_tensor *input = ggml_new_tensor_2d(ctx, GGML_TYPE_F32, k, m);
                float *x = (float *)input->data;
                for (int i = 0; i < m * k; i++) x[i] = random_normal();
                struct ggml_tensor *output = ggml_mul_mat(ctx, weight, input);
                struct ggml_cgraph *graph = ggml_new_graph(ctx);
                ggml_build_forward_expand(graph, output);
                struct ggml_cplan plan = ggml_graph_plan(graph, threads, pool);
                plan.work_data = malloc(plan.work_size > 0 ? plan.work_size : 1);
                ggml_graph_compute(graph, &plan);
                ggml_graph_compute(graph, &plan);
                static double times[2000];
                int count = 0;
                int64_t start = ggml_time_us();
                while (count < 5 || (ggml_time_us() - start < 500000 && count < 2000)) {
                    int64_t begin = ggml_time_us();
                    ggml_graph_compute(graph, &plan);
                    times[count++] = (ggml_time_us() - begin) / 1000.0;
                }
                qsort(times, count, sizeof(double), compare);
                double ms = times[count / 2];
                double gflops = 2.0 * (double)m * n * k / (ms * 1e6);
                printf("{\"lib\":\"ggml\",\"type\":\"%s\",\"m\":%d,\"n\":%d,\"k\":%d,\"threads\":\"%d\",\"ms\":%f,\"gflops\":%f}\n",
                       names[t], m, n, k, threads, ms, gflops);
                fflush(stdout);
                free(plan.work_data);
                ggml_free(ctx);
            }
        }
        free(source);
    }
    ggml_threadpool_free(pool);
    return 0;
}
