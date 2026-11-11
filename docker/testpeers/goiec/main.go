// ffgoiec: a small client over otfabric/go-iec61850 (MIT; an MMS/GOOSE stack independent of libiec61850 and of iec61850-rs).
// Usage: ffgoiec ADDR IFACE [REF...]. Browses the model, reads every data set and the GoCB, reads each REF, and subscribes
// to GOOSE on IFACE for a few seconds. Prints "name: value" lines for conformance/iec61850/check.sh and exits 1 on any error.
package main

import (
	"context"
	"fmt"
	"os"
	"time"

	iec61850 "github.com/otfabric/go-iec61850"
	"github.com/otfabric/go-iec61850/goose"
)

var bad int

func chk(what string, err error) bool {
	if err != nil {
		fmt.Printf("ERROR %s: %v\n", what, err)
		bad++
	}
	return err == nil
}

func main() {
	ctx, cancel := context.WithTimeout(context.Background(), 40*time.Second)
	defer cancel()
	addr, iface := os.Args[1], os.Args[2]
	n := make(chan int, 1)
	go func() { n <- gooseRun(ctx, iface) }()
	c, err := iec61850.Dial(ctx, addr, iec61850.DialOptions{})
	if !chk("dial", err) {
		os.Exit(1)
	}
	defer c.Close(ctx)
	lds, err := c.ListLogicalDevices(ctx)
	chk("ListLogicalDevices", err)
	for _, ld := range lds {
		fmt.Println("LD:", ld.Name)
		lns, err := c.ListLogicalNodes(ctx, ld.Name)
		chk("ListLogicalNodes", err)
		for _, ln := range lns {
			dos, err := c.ListDataObjects(ctx, ld.Name, ln.Name)
			chk("ListDataObjects "+ln.Name, err)
			fmt.Println("LN:", ln.Name, "data objects:", len(dos))
		}
		dss, err := c.ListDataSets(ctx, ld.Name)
		chk("ListDataSets", err)
		for _, ds := range dss {
			v, err := c.ReadDataSet(ctx, ld.Name, ds)
			chk("ReadDataSet "+ds, err)
			fmt.Printf("DATASET: %s members: %d\n", ds, len(v))
		}
		g, err := c.GetGoCB(ctx, ld.Name, "LLN0$GO$gcbAnalogValues")
		if chk("GetGoCB", err) {
			fmt.Printf("GOCB: goena=%v datset=%q confrev=%d\n", g.GoEna, g.DatSet, g.ConfRev)
		}
	}
	for _, r := range os.Args[3:] {
		ref, err := iec61850.ParseRef(r)
		if !chk("ParseRef "+r, err) {
			continue
		}
		v, err := c.Read(ctx, ref)
		if chk("Read "+r, err) {
			fmt.Printf("READ: %s = %v\n", r, v)
		}
	}
	got := <-n
	fmt.Println("GOOSE valid frames:", got)
	if got < 3 {
		fmt.Println("ERROR fewer than 3 GOOSE frames decoded")
		bad++
	}
	if bad > 0 {
		os.Exit(1)
	}
	fmt.Println("FFGOIEC OK")
}

// gooseRun subscribes on iface for 8 s and returns the number of frames that decoded without error.
func gooseRun(ctx context.Context, iface string) int {
	tr, err := goose.Open(iface, goose.TransportOptions{})
	if !chk("goose.Open", err) {
		return 0
	}
	sub, err := goose.NewSubscriber(tr, goose.SubscriberConfig{})
	if !chk("goose.NewSubscriber", err) {
		return 0
	}
	s, err := sub.Subscribe(goose.SubscriptionConfig{})
	if !chk("goose.Subscribe", err) {
		return 0
	}
	ctx, cancel := context.WithTimeout(ctx, 8*time.Second)
	defer cancel()
	go func() { _ = sub.Run(ctx) }()
	ok := 0
	for {
		select {
		case <-ctx.Done():
			return ok
		case ev := <-s.Events():
			if ev.Err != nil || ev.Frame == nil {
				continue
			}
			p := &ev.Frame.PDU
			if ok == 0 {
				fmt.Printf("GOOSE: gocbRef=%q datSet=%q appid=%v confRev=%d entries=%d\n", p.GocbRef, p.DatSet, ev.Frame.AppID, p.ConfRev, p.NumDatSetEntries)
			}
			ok++
		}
	}
}
