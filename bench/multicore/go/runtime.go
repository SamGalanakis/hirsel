package main

import (
	"fmt"
	"os"
	"runtime"
	"strconv"
	"sync"
)

const lanes = 16
const queue = 64

func spawnJoin(rounds uint64, lane uint64) uint64 {
	var sum uint64
	done := make(chan uint64)
	for i := uint64(0); i < rounds; i++ {
		go func(value uint64) { done <- value }(lane*rounds + i + 1)
		sum += <-done
	}
	return sum
}

func spawnChain(rounds uint64) uint64 {
	done := make(chan uint64, lanes)
	for lane := uint64(0); lane < lanes; lane++ {
		go func(lane uint64) { done <- spawnJoin(rounds, lane) }(lane)
	}
	var sum uint64
	for range lanes {
		sum += <-done
	}
	return sum
}

func pingPong(rounds uint64) uint64 {
	send, reply := make(chan uint64), make(chan uint64)
	done := make(chan struct{})
	go func() {
		defer close(done)
		for range rounds {
			reply <- (<-send + 1)
		}
	}()
	var sum uint64
	for i := uint64(0); i < rounds; i++ {
		send <- i
		sum += <-reply
	}
	<-done
	return sum
}

func yieldMany(rounds uint64) uint64 {
	done := make(chan uint64, lanes)
	for lane := uint64(0); lane < lanes; lane++ {
		go func(lane uint64) {
			var sum uint64
			for i := uint64(0); i < rounds; i++ {
				runtime.Gosched()
				sum += lane*rounds + i + 1
			}
			done <- sum
		}(lane)
	}
	var sum uint64
	for range lanes {
		sum += <-done
	}
	return sum
}

func fanIn(rounds uint64) uint64 {
	ch := make(chan uint64, queue)
	var producers sync.WaitGroup
	for lane := uint64(0); lane < lanes; lane++ {
		producers.Go(func() {
			for i := uint64(0); i < rounds; i++ {
				ch <- lane*rounds + i + 1
			}
		})
	}
	go func() {
		producers.Wait()
		close(ch)
	}()
	var sum uint64
	for value := range ch {
		sum += value
	}
	return sum
}

func main() {
	workers, err := strconv.Atoi(os.Args[2])
	if err != nil || workers < 1 {
		panic("invalid worker count")
	}
	runtime.GOMAXPROCS(workers)
	rounds, err := strconv.ParseUint(os.Args[3], 10, 64)
	if err != nil {
		panic(err)
	}
	if len(os.Args) == 5 {
		completed, checksum := runGrain(os.Args[1], rounds, os.Args[4])
		fmt.Println(completed, checksum)
		return
	}
	var sum uint64
	switch os.Args[1] {
	case "spawn_join":
		sum = spawnJoin(rounds, 0)
	case "spawn_chain":
		sum = spawnChain(rounds)
	case "ping_pong":
		sum = pingPong(rounds)
	case "yield_many":
		sum = yieldMany(rounds)
	case "fan_in":
		sum = fanIn(rounds)
	default:
		panic("unknown workload")
	}
	fmt.Println(sum)
}
