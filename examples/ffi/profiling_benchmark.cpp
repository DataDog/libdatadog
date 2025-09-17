// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <benchmark/benchmark.h>
#include <datadog/common.h>
#include <datadog/profiling.h>
#include <vector>
#include <random>
#include <string>
#include <chrono>
#include <cstdio>
#include <cstdlib>

// Helper function to check status and exit on error
static void check_ok(struct ddog_prof_Status status, const char *context) {
    if (status.flags != 0) {
        const char *msg = status.err ? status.err : "(unknown)";
        fprintf(stderr, "%s: %s\n", context, msg);
        ddog_prof_Status_drop(&status);
        exit(EXIT_FAILURE);
    }
}

// Random stack generator
class StackGenerator {
public:
    std::mt19937 rng_;
    std::uniform_int_distribution<int> function_dist_;

private:
    std::uniform_int_distribution<int> stack_depth_dist_;
    std::uniform_int_distribution<int> file_dist_;
    std::uniform_int_distribution<int64_t> value_dist_;
    
    std::vector<std::string> function_names_;
    std::vector<std::string> file_names_;
    
public:
    StackGenerator() 
        : rng_(std::chrono::steady_clock::now().time_since_epoch().count())
        , stack_depth_dist_(1, 20)  // Stack depth 1-20
        , function_dist_(0, 2999)   // 3000 functions
        , file_dist_(0, 99)         // 100 files
        , value_dist_(1, 1000000)   // Sample values 1-1M
    {
        // Generate function names
        function_names_.reserve(3000);
        for (int i = 0; i < 3000; ++i) {
            function_names_.push_back("function_" + std::to_string(i));
        }
        
        // Generate file names
        file_names_.reserve(100);
        for (int i = 0; i < 100; ++i) {
            file_names_.push_back("/path/to/file_" + std::to_string(i) + ".cpp");
        }
    }
    
    int getStackDepth() { return stack_depth_dist_(rng_); }
    const std::string& getRandomFunction() { return function_names_[function_dist_(rng_)]; }
    const std::string& getRandomFile() { return file_names_[file_dist_(rng_)]; }
    int64_t getRandomValue() { return value_dist_(rng_); }
};

// Benchmark fixture
class ProfilingBenchmark : public benchmark::Fixture {
protected:
    ddog_prof_ProfilesDictionaryHandle dict_;
    ddog_prof_ScratchPadHandle scratch_;
    ddog_prof_ProfileHandle profile_;
    ddog_prof_ValueType wall_time_vt_;
    StackGenerator stack_gen_;
    
    // Pre-created function and mapping IDs to avoid overhead during benchmark
    std::vector<ddog_prof_FunctionId> function_ids_;
    ddog_prof_MappingId mapping_id_;
    
public:
    void SetUp(const ::benchmark::State& state) override {
        // Initialize core handles
        dict_ = nullptr;
        scratch_ = nullptr;
        profile_ = nullptr;
        
        check_ok(ddog_prof_ProfilesDictionary_new(&dict_), "ProfilesDictionary_new");
        check_ok(ddog_prof_ScratchPad_new(&scratch_), "ScratchPad_new");
        check_ok(ddog_prof_Profile_new(&profile_), "Profile_new");
        
        // Setup value type for wall-time
        ddog_prof_StringId vt_type, vt_unit;
        check_ok(ddog_prof_ProfilesDictionary_insert_str(&vt_type, dict_, 
                 DDOG_CHARSLICE_C("wall-time"), DDOG_PROF_UTF8_OPTION_VALIDATE),
                 "insert_str(wall-time)");
        check_ok(ddog_prof_ProfilesDictionary_insert_str(&vt_unit, dict_, 
                 DDOG_CHARSLICE_C("nanoseconds"), DDOG_PROF_UTF8_OPTION_VALIDATE),
                 "insert_str(nanoseconds)");
        
        wall_time_vt_ = {.type_id = vt_type, .unit_id = vt_unit};
        check_ok(ddog_prof_Profile_add_sample_type(profile_, wall_time_vt_), "add_sample_type");
        check_ok(ddog_prof_Profile_add_period(profile_, 1000000000LL, wall_time_vt_), "add_period");
        
        // Pre-create functions to avoid string insertion overhead during benchmark
        function_ids_.reserve(3000);
        for (int i = 0; i < 3000; ++i) {
            ddog_prof_Function func = {.system_name = DDOG_PROF_STRINGID_EMPTY};
            
            std::string func_name = "function_" + std::to_string(i);
            std::string file_name = "/path/to/file_" + std::to_string(i % 100) + ".cpp";
            
            check_ok(ddog_prof_ProfilesDictionary_insert_str(&func.name, dict_,
                     {func_name.c_str(), func_name.length()}, DDOG_PROF_UTF8_OPTION_VALIDATE),
                     "insert_str(func_name)");
            check_ok(ddog_prof_ProfilesDictionary_insert_str(&func.file_name, dict_,
                     {file_name.c_str(), file_name.length()}, DDOG_PROF_UTF8_OPTION_VALIDATE),
                     "insert_str(file_name)");
            
            ddog_prof_FunctionId func_id;
            check_ok(ddog_prof_ProfilesDictionary_insert_function(&func_id, dict_, &func), 
                     "insert_function");
            function_ids_.push_back(func_id);
        }
        
        // Create a single mapping for all locations
        ddog_prof_Mapping mapping = {.build_id = DDOG_PROF_STRINGID_EMPTY};
        check_ok(ddog_prof_ProfilesDictionary_insert_str(&mapping.filename, dict_,
                 DDOG_CHARSLICE_C("/bin/benchmark"), DDOG_PROF_UTF8_OPTION_VALIDATE),
                 "insert_str(mapping)");
        check_ok(ddog_prof_ProfilesDictionary_insert_mapping(&mapping_id_, dict_, &mapping), 
                 "insert_mapping");
    }
    
    void TearDown(const ::benchmark::State& state) override {
        if (profile_) ddog_prof_Profile_drop(&profile_);
        if (scratch_) ddog_prof_ScratchPad_drop(&scratch_);
        if (dict_) ddog_prof_ProfilesDictionary_drop(&dict_);
    }
    
    // Generate a random stack and create a sample
    void CreateRandomSample() {
        int stack_depth = stack_gen_.getStackDepth();
        std::vector<ddog_prof_LocationId> location_ids;
        location_ids.reserve(stack_depth);
        
        // Create locations for this stack
        for (int i = 0; i < stack_depth; ++i) {
            int func_idx = stack_gen_.function_dist_(stack_gen_.rng_) % function_ids_.size();
            ddog_prof_FunctionId func_id = function_ids_[func_idx];
            
            ddog_prof_Line line = {.line_number = static_cast<int64_t>(i + 1), .function_id = func_id};
            ddog_prof_Location loc = {.address = static_cast<uint64_t>(i * 0x1000), 
                                     .mapping_id = mapping_id_, .line = line};
            
            ddog_prof_LocationId loc_id;
            check_ok(ddog_prof_ScratchPad_insert_location(&loc_id, scratch_, &loc),
                     "insert_location");
            location_ids.push_back(loc_id);
        }
        
        // Create stack from locations
        ddog_prof_Slice_LocationId loc_slice = {location_ids.data(), location_ids.size()};
        ddog_prof_StackId stack_id;
        check_ok(ddog_prof_ScratchPad_insert_stack(&stack_id, scratch_, loc_slice),
                 "insert_stack");
        
        // Create sample
        ddog_prof_SampleBuilderHandle sb = nullptr;
        check_ok(ddog_prof_SampleBuilder_new(&sb, scratch_), "SampleBuilder_new");
        check_ok(ddog_prof_SampleBuilder_stack_id(sb, stack_id), "SampleBuilder_stack_id");
        check_ok(ddog_prof_SampleBuilder_value(sb, stack_gen_.getRandomValue()), "SampleBuilder_value");
        check_ok(ddog_prof_SampleBuilder_build_into_profile(&sb, profile_),
                 "SampleBuilder_build_into_profile");
    }
};

// Benchmark: Aggregate samples continuously
BENCHMARK_DEFINE_F(ProfilingBenchmark, AggregateSamples)(benchmark::State& state) {
    int64_t total_samples = 0;
    
    for (auto _ : state) {
        CreateRandomSample();
        total_samples++;
    }
    
    state.counters["samples_per_sec"] = benchmark::Counter(total_samples, benchmark::Counter::kIsRate);
    state.counters["total_samples"] = total_samples;
}

// Benchmark: Race to aggregate 100k samples
BENCHMARK_DEFINE_F(ProfilingBenchmark, Race100kSamples)(benchmark::State& state) {
    const int target_samples = 100000;
    
    for (auto _ : state) {
        // Reset profile for each iteration
        if (profile_) ddog_prof_Profile_drop(&profile_);
        check_ok(ddog_prof_Profile_new(&profile_), "Profile_new");
        check_ok(ddog_prof_Profile_add_sample_type(profile_, wall_time_vt_), "add_sample_type");
        check_ok(ddog_prof_Profile_add_period(profile_, 1000000000LL, wall_time_vt_), "add_period");
        
        auto start = std::chrono::high_resolution_clock::now();
        
        for (int i = 0; i < target_samples; ++i) {
            CreateRandomSample();
        }
        
        auto end = std::chrono::high_resolution_clock::now();
        auto duration = std::chrono::duration_cast<std::chrono::milliseconds>(end - start);
        
        state.counters["duration_ms"] = duration.count();
        state.counters["samples_per_sec"] = benchmark::Counter(target_samples * 1000.0 / duration.count());
    }
}

// Benchmark: Build pprof after aggregation
BENCHMARK_DEFINE_F(ProfilingBenchmark, BuildPprof)(benchmark::State& state) {
    // Pre-populate with samples
    const int num_samples = state.range(0);
    for (int i = 0; i < num_samples; ++i) {
        CreateRandomSample();
    }
    
    for (auto _ : state) {
        ddog_prof_PprofBuilderHandle pprof = nullptr;
        check_ok(ddog_prof_PprofBuilder_new(&pprof, dict_, scratch_), "PprofBuilder_new");
        check_ok(ddog_prof_PprofBuilder_add_profile(pprof, profile_), "add_profile");
        
        ddog_prof_EncodedProfile encoded = {0};
        struct ddog_Timespec start_time = {.seconds = 0, .nanoseconds = 0};
        struct ddog_Timespec end_time = {.seconds = 10, .nanoseconds = 0};
        
        check_ok(ddog_prof_PprofBuilder_build_compressed(&encoded, pprof, 4096, start_time, end_time),
                 "build_compressed");
        
        ddog_prof_PprofBuilder_drop(&pprof);
        // Note: We should drop encoded profile but the API doesn't show how
    }
    
    state.counters["input_samples"] = num_samples;
}

// Register benchmarks
BENCHMARK_REGISTER_F(ProfilingBenchmark, AggregateSamples)
    ->Unit(benchmark::kMillisecond)
    ->MinTime(10.0)  // Run for at least 10 seconds
    ->UseRealTime();

BENCHMARK_REGISTER_F(ProfilingBenchmark, Race100kSamples)
    ->Unit(benchmark::kMillisecond)
    ->Iterations(5)  // Run 5 races
    ->UseRealTime();

BENCHMARK_REGISTER_F(ProfilingBenchmark, BuildPprof)
    ->Unit(benchmark::kMillisecond)
    ->Arg(1000)    // 1k samples
    ->Arg(10000)   // 10k samples
    ->Arg(50000)   // 50k samples
    ->Arg(100000)  // 100k samples
    ->UseRealTime();

BENCHMARK_MAIN();
