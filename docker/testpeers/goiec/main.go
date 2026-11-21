// ffgoiec: a small client over otfabric/go-iec61850 (MIT; an MMS/GOOSE stack independent of libiec61850 and of iec61850-rs).
// Usage: ffgoiec ADDR IFACE [REF...]. Browses the model, reads every data set and the GoCB, reads each REF, and subscribes
// to GOOSE on IFACE for a few seconds. Prints "name: value" lines for conformance/iec61850/check.sh and exits 1 on any error.
// ffgoiec publish IFACE SCL IED LD GOCB N: publishes that GSEControl as the SCL file describes it (address, APPID, data set),
// starting from the members' zero values and toggling the first boolean member N times, one state a second
// (conformance/iec61850-rtu). Prints "PUBLISH stNum n: ref = value" per state.
package main

import (
	"context"
	"fmt"
	"os"
	"strconv"
	"time"

	iec61850 "github.com/otfabric/go-iec61850"
	"github.com/otfabric/go-iec61850/goose"
	"github.com/otfabric/go-iec61850/scl"
	mms "github.com/otfabric/go-mms"
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
	if len(os.Args) == 8 && os.Args[1] == "publish" {
		os.Exit(publish(os.Args[2:]))
	}
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

// publish runs `ffgoiec publish IFACE SCL IED LD GOCB N`; the exit status is 1 on any error.
func publish(a []string) int {
	n, err := strconv.Atoi(a[5])
	if !chk("count", err) {
		return 1
	}
	doc, err := scl.ParseFile(a[1])
	if !chk("scl.ParseFile", err) {
		return 1
	}
	cb, err := goose.ControlBlockFromSCL(doc, a[2], a[3], a[4])
	if !chk("ControlBlockFromSCL", err) {
		return 1
	}
	cfg, err := cb.PublisherConfig()
	if !chk("PublisherConfig", err) {
		return 1
	}
	tr, err := goose.Open(a[0], goose.TransportOptions{})
	if !chk("goose.Open", err) {
		return 1
	}
	defer func() { _ = tr.Close() }()
	cfg.Src = tr.HardwareAddr()
	pub, err := goose.NewPublisher(tr, cfg)
	if !chk("NewPublisher", err) {
		return 1
	}
	defer func() { _ = pub.Close() }()
	values, at := cfg.Values, -1
	for i, v := range values {
		if _, ok := v.Bool(); ok {
			at = i
			break
		}
	}
	if at < 0 {
		fmt.Printf("ERROR %s: no boolean member\n", cb.DatSet)
		return 1
	}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	go func() { _ = pub.Run(ctx) }()
	fmt.Printf("PUBLISH stNum %d: %s = false\n", pub.State().StNum, cb.Members[at].Ref)
	for i := 0; i < n; i++ {
		time.Sleep(time.Second)
		b, _ := values[at].Bool()
		values[at] = mms.NewBoolean(!b)
		if !chk("Publish", pub.Publish(values)) {
			return 1
		}
		fmt.Printf("PUBLISH stNum %d: %s = %v\n", pub.State().StNum, cb.Members[at].Ref, !b)
	}
	time.Sleep(time.Second)
	return 0
}
