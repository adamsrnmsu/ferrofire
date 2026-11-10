-- Wireshark dissector for ferrofire datagram v1 (docs/datagram-v1.md): libzmq RADIO framing
--   [u8 group_len][group][body]   body = "tag:value,tag:value,...,_ff.seq:N,_ff.cyc:N,_ff.t:NS," + NUL
-- Install: copy to ~/.local/lib/wireshark/plugins/ (or run: tshark -X lua_script:ferrofire_datagram.lua ...).
-- It claims UDP port 40000; for another port use Analyze > Decode As > ferrofire.
local p = Proto("ferrofire", "ferrofire datagram v1")

local f = {
  group_len = ProtoField.uint8("ferrofire.group_len", "Group length"),
  group     = ProtoField.string("ferrofire.group", "Group"),
  body      = ProtoField.string("ferrofire.body", "Body"),
  point     = ProtoField.string("ferrofire.point", "Point"),
  tag       = ProtoField.string("ferrofire.tag", "Tag"),
  value     = ProtoField.string("ferrofire.value", "Value"),
  npoints   = ProtoField.uint32("ferrofire.points", "Points"),
  seq       = ProtoField.uint64("ferrofire.seq", "_ff.seq (datagram counter)"),
  cyc       = ProtoField.uint64("ferrofire.cyc", "_ff.cyc (publish cycle)"),
  t         = ProtoField.string("ferrofire.t", "_ff.t (send time, unix ns)"), -- ns since 1970 exceeds 2^53: keep the digits
  nul       = ProtoField.bool("ferrofire.nul", "NUL terminated"),
}
for _, v in pairs(f) do p.fields[#p.fields + 1] = v end

local expert = ProtoExpert.new("ferrofire.malformed", "Malformed ferrofire datagram",
                               expert.group.MALFORMED, expert.severity.WARN)
p.experts = { expert }

local pseudo = { ["_ff.seq"] = f.seq, ["_ff.cyc"] = f.cyc, ["_ff.t"] = f.t }

function p.dissector(buf, pinfo, tree)
  local n = buf:len()
  if n < 1 then return 0 end
  local glen = buf(0, 1):uint()
  if glen > n - 1 then return 0 end
  pinfo.cols.protocol = "ferrofire"
  local t = tree:add(p, buf(), "ferrofire datagram v1")
  t:add(f.group_len, buf(0, 1))
  t:add(f.group, buf(1, glen))
  local blen = n - 1 - glen
  if blen == 0 then
    t:add_proto_expert_info(expert, "empty body")
    return n
  end
  local raw = buf(1 + glen, blen):raw()
  local nul = raw:sub(-1) == "\0"
  t:add(f.nul, buf(n - 1, 1), nul)
  if not nul then t:add_proto_expert_info(expert, "truncated: no NUL terminator") end
  local text = nul and raw:sub(1, -2) or raw
  local base = 1 + glen
  t:add(f.body, buf(base, #text), text)
  local pos, count, seq = 1, 0, nil
  while pos <= #text do
    local c = text:find(",", pos, true)
    if not c then
      t:add_proto_expert_info(expert, "truncated: last point has no comma")
      break
    end
    local part = text:sub(pos, c - 1)
    local name, val = part:match("^([^:]*):(.*)$")
    local rng = buf(base + pos - 1, #part)
    if not name then
      t:add_proto_expert_info(expert, "point without ':' : " .. part)
    elseif pseudo[name] then
      local num = tonumber(val)
      if name == "_ff.t" then
        t:add(f.t, rng, val)
      elseif num then
        t:add(pseudo[name], rng, UInt64.new(num % 4294967296, num // 4294967296))
      end
      if name == "_ff.seq" then seq = val end
    else
      local pt = t:add(f.point, rng, part)
      pt:add(f.tag, rng, name)
      pt:add(f.value, rng, val)
      count = count + 1
    end
    pos = c + 1
  end
  t:add(f.npoints, buf(), count):set_generated()
  pinfo.cols.info = string.format("group %s, %d points%s", buf(1, glen):string(), count,
                                  seq and (", seq " .. seq) or "")
  return n
end

DissectorTable.get("udp.port"):add(40000, p)
