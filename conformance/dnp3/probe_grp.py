from dnp3probe import *
# ww-a12: READ of every variation 0..12 of the groups ferrofire answers without points (diff-server.xml), so the table of which
# group and variation is empty, unreadable or unknown comes from stock and not from memory.
for g in (3, 4, 11, 12, 13, 20, 21, 22, 23, 41, 42, 50, 51, 52, 80):
    print("== group %d" % g)
    for v in range(13):
        req("g%dv%d" % (g, v), 1, hdr(g, v, 6), wait=0.25)
