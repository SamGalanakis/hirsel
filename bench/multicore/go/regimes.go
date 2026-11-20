package main

import (
	"fmt"
	"os"
	"strconv"
	"time"
)

//go:noinline
func kernel(x, rounds uint64) uint64 {
	for range rounds {
		x ^= x << 13
		x ^= x >> 7
		x ^= x << 17
	}
	return x
}

func grainSum(jobs []chan uint64) uint64 {
	var sum uint64
	for i := len(jobs) - 1; i >= 0; i-- {
		sum += <-jobs[i]
	}
	return sum
}

func grainJob(work func() uint64) chan uint64 {
	done := make(chan uint64, 1)
	go func() { done <- work() }()
	return done
}

func grainIndependent(lanes, waves, grain uint64) uint64 {
	var sum uint64
	for wave := range waves {
		jobs := make([]chan uint64, 0, lanes)
		for lane := range lanes {
			jobs = append(jobs, grainJob(func() uint64 { return kernel(wave*lanes+lane+1, grain) }))
		}
		sum += grainSum(jobs)
	}
	return sum
}

func grainStage(input <-chan uint64, output chan<- uint64, count, grain uint64) <-chan struct{} {
	done := make(chan struct{})
	go func() {
		defer close(done)
		for range count {
			output <- kernel(<-input, grain)
		}
	}()
	return done
}

func grainPair(start, count, grain uint64) uint64 {
	request, reply := make(chan uint64, 1), make(chan uint64, 1)
	done := grainStage(request, reply, count, grain)
	var sum uint64
	for i := range count {
		request <- start + i
		sum += <-reply
	}
	<-done
	return sum
}

func grainChain(start, count, grain uint64) uint64 {
	var sum uint64
	for i := range count {
		sum += <-grainJob(func() uint64 { return kernel(start+i, grain) })
	}
	return sum
}

func grainLanes(count, grain uint64, work func(uint64, uint64, uint64) uint64) uint64 {
	jobs := make([]chan uint64, 0, lanes)
	for lane := uint64(0); lane < lanes; lane++ {
		jobs = append(jobs, grainJob(func() uint64 { return work(lane*count+1, count, grain) }))
	}
	return grainSum(jobs)
}

func grainProduce(output chan<- uint64, start, count, grain uint64) <-chan struct{} {
	done := make(chan struct{})
	go func() {
		defer close(done)
		for i := range count {
			output <- kernel(start+i, grain)
		}
	}()
	return done
}

func grainCollect(input <-chan uint64, count uint64, jobs []<-chan struct{}) uint64 {
	var sum uint64
	for range count {
		sum += <-input
	}
	for i := len(jobs) - 1; i >= 0; i-- {
		<-jobs[i]
	}
	return sum
}

func grainFanIn(count, grain uint64) uint64 {
	output := make(chan uint64, queue)
	jobs := make([]<-chan struct{}, 0, lanes)
	for lane := uint64(0); lane < lanes; lane++ {
		jobs = append(jobs, grainProduce(output, lane*count+1, count, grain))
	}
	return grainCollect(output, lanes*count, jobs)
}

func grainFanOut(count, grain uint64) uint64 {
	output := make(chan uint64, queue)
	jobs := make([]<-chan struct{}, 0, lanes+1)
	inputs := make([]chan uint64, lanes)
	for lane := range inputs {
		inputs[lane] = make(chan uint64, queue)
		jobs = append(jobs, grainStage(inputs[lane], output, count, grain))
	}
	done := make(chan struct{})
	go func() {
		defer close(done)
		for i := range count {
			for lane, input := range inputs {
				input <- i*lanes + uint64(lane) + 1
			}
		}
	}()
	return grainCollect(output, lanes*count, append(jobs, done))
}

func grainPipeline(count, grain uint64) uint64 {
	sender := make(chan uint64, queue)
	input := sender
	jobs := make([]<-chan struct{}, 0, 9)
	for range 8 {
		output := make(chan uint64, queue)
		jobs = append(jobs, grainStage(input, output, count, grain))
		input = output
	}
	return grainCollect(input, count, append(jobs, grainProduce(sender, 1, count, 0)))
}

func grainTree(start, leaves, grain uint64) uint64 {
	if leaves == 1 {
		return kernel(start, grain)
	}
	half := leaves / 2
	a := grainJob(func() uint64 { return grainTree(start, half, grain) })
	b := grainJob(func() uint64 { return grainTree(start+half, half, grain) })
	return <-a + <-b
}

func grainForest(waves, grain uint64) uint64 {
	var sum uint64
	for wave := range waves {
		sum += grainTree(wave*64+1, 64, grain)
	}
	return sum
}

func grainHub(count, grain uint64) uint64 {
	request := make(chan int, 1)
	release := make(chan struct{}, 1)
	replies := make([]chan struct{}, lanes)
	jobs := make([]chan uint64, 0, lanes)
	for lane := range replies {
		grant := make(chan struct{}, 1)
		replies[lane] = grant
		jobs = append(jobs, grainJob(func() uint64 {
			var sum uint64
			for i := range count {
				value := kernel(uint64(lane)*count+i+1, grain)
				request <- lane
				<-grant
				sum += value
				release <- struct{}{}
			}
			return sum
		}))
	}
	done := make(chan struct{})
	go func() {
		defer close(done)
		for range lanes * count {
			replies[<-request] <- struct{}{}
			<-release
		}
	}()
	sum := grainSum(jobs)
	<-done
	return sum
}

func grainTimers(count, grain, hot uint64) uint64 {
	type wake struct {
		sum uint64
		ns  uint64
	}
	var hotJob chan wake
	if hot != 0 {
		hotJob = make(chan wake, 1)
		go func() {
			start := time.Now()
			sum := grainPair(count+1, hot, grain)
			hotJob <- wake{sum, uint64(time.Since(start).Nanoseconds())}
		}()
	}
	jobs := make([]chan wake, 0, count)
	for i := range count {
		done := make(chan wake, 1)
		jobs = append(jobs, done)
		go func() {
			deadline := time.Now().Add(time.Duration(2000+i) * time.Microsecond)
			time.Sleep(time.Until(deadline))
			lag := max(0, time.Since(deadline).Nanoseconds())
			rounds := grain
			if hot != 0 {
				rounds = 0
			}
			done <- wake{kernel(i+1, rounds), uint64(lag)}
		}()
	}
	var sum, lag, worst uint64
	for i := len(jobs) - 1; i >= 0; i-- {
		result := <-jobs[i]
		sum += result.sum
		lag += result.ns
		worst = max(worst, result.ns)
	}
	if hotJob != nil {
		result := <-hotJob
		sum += result.sum
		fmt.Fprintf(os.Stderr, "hot-ns=%d\n", result.ns)
	}
	fmt.Fprintf(os.Stderr, "wake-mean-ns=%d\nwake-max-ns=%d\n", lag/count, worst)
	return sum
}

func runGrain(name string, count uint64, arg string) (uint64, uint64) {
	grain, err := strconv.ParseUint(arg, 10, 64)
	if err != nil {
		panic(err)
	}
	switch name {
	case "independent16":
		return 16 * count, grainIndependent(16, count, grain)
	case "independent256":
		return 256 * count, grainIndependent(256, count, grain)
	case "pairs":
		return lanes * count, grainLanes(count, grain, grainPair)
	case "single_pair":
		return count, grainPair(1, count, grain)
	case "fanin":
		return lanes * count, grainFanIn(count, grain)
	case "fanout":
		return lanes * count, grainFanOut(count, grain)
	case "pipeline":
		return count, grainPipeline(count, grain)
	case "grain_spawn_join":
		return count, grainChain(1, count, grain)
	case "grain_spawn_chain":
		return lanes * count, grainLanes(count, grain, grainChain)
	case "spawn_tree":
		return count * 64, grainForest(count, grain)
	case "channel_hub":
		return lanes * count, grainHub(count, grain)
	case "skew":
		return 1000 + count, grainTimers(1000, grain, count)
	case "timers":
		return count, grainTimers(count, grain, 0)
	case "calibrate":
		started := time.Now()
		var sum uint64
		for i := range count {
			sum += kernel(i+1, grain)
		}
		fmt.Fprintf(os.Stderr, "kernel-ns=%d\n", time.Since(started).Nanoseconds())
		return count, sum
	default:
		panic("unknown grain workload")
	}
}
