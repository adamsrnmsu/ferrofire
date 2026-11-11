-- Wireshark / tshark post-dissector for ICCP / TASE.2 (IEC 60870-6) on top of the built-in MMS dissector.
-- Install: copy to ~/.local/lib/wireshark/plugins/ or run: tshark -X lua_script:iccp.lua -r x.pcap -Y iccp
-- It re-reads the MMS PDU bytes of each frame that the MMS dissector decoded, walks the BER, and adds an "ICCP / TASE.2"
-- tree, iccp.* fields and an "ICCP:" info-column suffix. Only connections that mention a TASE.2 object (Bilateral_Table_ID,
-- TASE2_Version, ... or a transfer report against a named list) are tagged, so plain IEC 61850 MMS is left alone.
-- The interpretation mirrors tools/zeek/iccp/analyzer/iccp.spicy and scripts/main.zeek; keep the tables in sync.
-- TASE.2 names and encodings are as remembered from IEC 60870-6-503/802, not checked against a licensed copy
-- (tools/zeek/iccp/README.md, "Assumptions").
-- The MMS dissector needs the association (initiate) in the capture, or data-only packets do not decode as MMS.
local p = Proto("iccp", "ICCP / TASE.2")

local F = {
  pdu       = ProtoField.string("iccp.pdu", "MMS PDU"),
  service   = ProtoField.string("iccp.service", "MMS service"),
  invoke_id = ProtoField.int32("iccp.invoke_id", "Invoke ID"),
  is_tase2  = ProtoField.bool("iccp.is_tase2", "TASE.2 connection"),
  op        = ProtoField.string("iccp.op", "Operation"),
  object    = ProtoField.string("iccp.object", "Object"),
  class     = ProtoField.string("iccp.class", "Object class"),
  scope     = ProtoField.string("iccp.scope", "Scope"),
  domain    = ProtoField.string("iccp.domain", "Domain (ICC)"),
  type      = ProtoField.string("iccp.type", "Value type"),
  value     = ProtoField.string("iccp.value", "Value"),
  quality   = ProtoField.string("iccp.quality", "Quality"),
  result    = ProtoField.string("iccp.result", "Result"),
  tset      = ProtoField.string("iccp.transfer_set", "Transfer set"),
  dataset   = ProtoField.string("iccp.dataset", "Data set"),
  conds     = ProtoField.string("iccp.ds_conditions", "DSConditions detected"),
  timetag   = ProtoField.string("iccp.timetag", "Transfer set time stamp"),
  bt        = ProtoField.string("iccp.bilateral_table_id", "Bilateral table ID"),
  version   = ProtoField.string("iccp.tase2_version", "TASE.2 version"),
  features  = ProtoField.string("iccp.supported_features", "Supported features"),
}
for _, v in pairs(F) do p.fields[#p.fields + 1] = v end

-- Names that exist only in TASE.2 (class = lowercased name).
local special = {
  Bilateral_Table_ID = true, TASE2_Version = true, Supported_Features = true, Next_DSTransfer_Set = true,
  Transfer_Set_Name = true, Transfer_Set_Time_Stamp = true, DSConditions_Detected = true, Event_Code_Detected = true,
}
local bit_classes = { supported_features = true, dsconditions_detected = true }
local ds_condition_bits = { "Interval", "Integrity", "ObjectChange", "OperatorRequest", "OtherExternalEvent" }
local report_header = { "Transfer_Set_Name", "Transfer_Set_Time_Stamp", "DSConditions_Detected" }

-- The MMS dissector's PDU content fields: field name -> { PDU name, BER tag of the PDU CHOICE }.
local pdu_fields = {
  { "mms.confirmed_RequestPDU_element", "confirmed_req", 0xA0 }, { "mms.confirmed_ResponsePDU_element", "confirmed_resp", 0xA1 },
  { "mms.confirmed_ErrorPDU_element", "confirmed_err", 0xA2 }, { "mms.unconfirmed_PDU_element", "unconfirmed", 0xA3 },
  { "mms.initiate_RequestPDU_element", "initiate_req", 0xA8 }, { "mms.initiate_ResponsePDU_element", "initiate_resp", 0xA9 },
  { "mms.conclude_RequestPDU_element", "conclude_req", 0x8B }, { "mms.conclude_ResponsePDU_element", "conclude_resp", 0x8C },
}
for _, e in ipairs(pdu_fields) do e[4] = Field.new(e[1]) end
local f_stream = Field.new("tcp.stream")

-- ---- BER ---------------------------------------------------------------------------------------------------

local MAXD = 40

-- Parse the TLVs in s[i..j]; returns a list of nodes or nil on any structural error.
local function parse(s, i, j, depth)
  if depth > MAXD then return nil end
  local out = {}
  while i <= j do
    local tag = s:byte(i)
    if not tag or tag % 32 == 31 then return nil end
    local l = s:byte(i + 1)
    if not l or l == 0x80 or l == 0xFF then return nil end
    local pos, len = i + 2, l
    if l > 0x80 then
      local n = l - 0x80
      if n > 3 or pos + n - 1 > j then return nil end
      len = 0
      for k = 0, n - 1 do len = len * 256 + s:byte(pos + k) end
      pos = pos + n
    end
    if pos + len - 1 > j then return nil end
    local node = { tag = tag, d = s:sub(pos, pos + len - 1), kids = {} }
    if tag >= 0x20 and (tag & 0x20) ~= 0 then
      node.kids = parse(s, pos, pos + len - 1, depth + 1)
      if not node.kids then return nil end
    end
    out[#out + 1] = node
    i = pos + len
  end
  return out
end

local function int(d) -- signed big endian
  if #d == 0 or #d > 8 then return 0 end
  local v = d:byte(1)
  if v >= 128 then v = v - 256 end
  for k = 2, #d do v = v * 256 + d:byte(k) end
  return v
end

local function uint(d)
  local v = 0
  for k = 1, math.min(#d, 8) do v = v * 256 + d:byte(k) end
  return v
end

local function find(n, tag)
  for _, c in ipairs(n.kids) do if c.tag == tag then return c end end
end

local function bits(d)
  if #d < 1 or #d > 513 then return "" end
  local pad = d:byte(1)
  local n = (#d - 1) * 8
  if pad > 7 or n < pad then return "" end
  n = n - pad
  local t = {}
  for i = 0, n - 1 do t[#t + 1] = ((d:byte(2 + i // 8) >> (7 - i % 8)) & 1) == 1 and "1" or "0" end
  return table.concat(t)
end

local function nbits(n) return #n.d >= 1 and (#n.d - 1) * 8 - n.d:byte(1) or 0 end
local function byte0(n) return #n.d >= 2 and n.d:byte(2) or 0 end
local function hex(d) return (d:gsub(".", function(c) return string.format("%02X", c:byte()) end)) end

local VALIDITY = { [0] = "VALID", "HELD", "SUSPECT", "NOTVALID" }
local SOURCE = { [0] = "TELEMETERED", "CALCULATED", "ENTERED", "ESTIMATED" }
local STATE = { [0] = "BETWEEN", "OFF", "ON", "INVALID" }

-- Data_Flags / Data_StateQ byte, MSB first: [0-1] state (StateQ) or spare, [2-3] validity, [4-5] current source,
-- [6] normal value, [7] time stamp quality.
local function quality(f)
  return VALIDITY[(f >> 4) & 3] .. "|" .. SOURCE[(f >> 2) & 3] .. "|" .. (((f >> 1) & 1) == 0 and "NORMAL" or "ABNORMAL") ..
         "|" .. ((f & 1) == 0 and "TS_VALID" or "TS_SUSPECT")
end

local function scalar(d)
  local t, v = d.tag, d.d
  if t == 0x87 then
    if #v == 5 then return string.format("%.6g", (Struct.unpack(">f", v:sub(2)))) end
    if #v == 9 then return string.format("%.12g", (Struct.unpack(">d", v:sub(2)))) end
    return "?"
  end
  if t == 0x85 then return string.format("%d", int(v)) end
  if t == 0x86 then return (#v > 0 and #v <= 8) and string.format("%d", uint(v)) or "?" end
  if t == 0x83 then return (#v > 0 and v:byte(1) ~= 0) and "true" or "false" end
  if t == 0x84 then return bits(v) end
  if t == 0x8A or t == 0x90 or t == 0x8B then return v end
  if t == 0x89 or t == 0x8C then return hex(v) end
  if t == 0x91 then return #v >= 4 and string.format("utc:%d", uint(v:sub(1, 4))) or "?" end
  return string.format("tag_%02x", t)
end

local function is_num(d) return d.tag == 0x87 or d.tag == 0x85 or d.tag == 0x86 end

-- (type, value, quality) of one MMS Data by TASE.2 shape.
local function describe(d)
  local t = d.tag
  if t == 0x87 then return "real", scalar(d), "" end
  if t == 0x85 or t == 0x86 then return "discrete", scalar(d), "" end
  if t == 0x84 then
    if nbits(d) == 8 then return "stateq", bits(d.d), STATE[(byte0(d) >> 6) & 3] .. "|" .. quality(byte0(d)) end
    if nbits(d) == 2 then return "state", bits(d.d), STATE[(byte0(d) >> 6) & 3] end
    return "bitstring", bits(d.d), ""
  end
  if t == 0xA2 then
    local k, n = d.kids, #d.kids
    if n == 2 and is_num(k[1]) and k[2].tag == 0x84 and nbits(k[2]) == 8 then
      return (k[1].tag == 0x87 and "realq" or "discreteq"), scalar(k[1]), quality(byte0(k[2]))
    end
    if n == 3 and k[1].tag ~= 0xA2 and is_num(k[2]) and k[3].tag == 0x84 and nbits(k[3]) == 8 then
      return (k[2].tag == 0x87 and "realq" or "discreteq") .. "timetag", scalar(k[2]) .. "@" .. scalar(k[1]), quality(byte0(k[3]))
    end
    local s = {}
    for i, c in ipairs(k) do
      if i > 16 then s[#s + 1] = "..."; break end
      s[#s + 1] = c.tag == 0xA2 and "{..}" or scalar(c)
    end
    return "struct", "{" .. table.concat(s, ";") .. "}", ""
  end
  if t == 0xA1 then return "array", string.format("%d items", #d.kids), "" end
  if t == 0x8A or t == 0x90 then return "string", scalar(d), "" end
  if t == 0x83 then return "boolean", scalar(d), "" end
  if t == 0x89 then return "octets", scalar(d), "" end
  if t == 0x91 or t == 0x8C or t == 0x8B then return "time", scalar(d), "" end
  return "other", scalar(d), ""
end

-- ---- MMS interpretation (same shape as ICCP::Spdu in iccp.spicy) -------------------------------------------

local function name_string(n)
  if n.tag == 0xA1 and #n.kids >= 2 then return n.kids[1].d .. "/" .. n.kids[2].d end
  if n.tag == 0x82 then return "aa:" .. n.d end
  return n.d
end

local function setname(r, n, as_target)
  local s = name_string(n)
  if as_target then r.target = s else r.names[#r.names + 1] = s end
  if r.scope == "" then
    r.scope = n.tag == 0x80 and "vmd" or (n.tag == 0xA1 and "domain" or "aa")
    if n.tag == 0xA1 and n.kids[1] then r.domain = n.kids[1].d end
  end
end

local function varspec(r, v)
  if v.tag == 0xA0 then
    for _, e in ipairs(v.kids) do
      local a = e.kids[1]
      if a and a.tag == 0xA0 and a.kids[1] then setname(r, a.kids[1], false) else r.names[#r.names + 1] = "?" end
    end
  elseif v.tag == 0xA1 and v.kids[1] then
    setname(r, v.kids[1], true)
  end
end

local function results(r, l)
  for _, d in ipairs(l.kids) do
    if d.tag == 0x80 then
      r.vtypes[#r.vtypes + 1], r.values[#r.values + 1], r.quals[#r.quals + 1] = "error", string.format("%d", int(d.d)), ""
    else
      local t, v, q = describe(d)
      r.vtypes[#r.vtypes + 1], r.values[#r.values + 1], r.quals[#r.quals + 1] = t, v, q
    end
  end
end

local function confirmed(r, req, s)
  local t = s.tag
  if t == 0xA4 then
    r.service = "read"
    if req then
      local a = find(s, 0xA1)
      if a and a.kids[1] then varspec(r, a.kids[1]) end
    else
      local a = find(s, 0xA1)
      if a then results(r, a) end
    end
  elseif t == 0xA5 then
    r.service = "write"
    if req then
      if #s.kids >= 2 then varspec(r, s.kids[1]); results(r, s.kids[2]) end
    else
      for _, c in ipairs(s.kids) do
        r.vtypes[#r.vtypes + 1], r.quals[#r.quals + 1] = "result", ""
        r.values[#r.values + 1] = c.tag == 0x81 and "success" or string.format("failure:%d", int(c.d))
      end
    end
  elseif t == 0xA1 then
    r.service = "getNameList"
    if not req then
      for _, c in ipairs(s.kids) do
        if c.tag == 0xA0 then for _, n in ipairs(c.kids) do r.names[#r.names + 1] = n.d end end
      end
    end
    r.listing = not req
  elseif t == 0xAB then
    r.service = "defineNamedVariableList"
    if req and #s.kids >= 2 then setname(r, s.kids[1], true); varspec(r, s.kids[2]) end
  elseif t == 0x8B then
    r.service = "defineNamedVariableList"
  elseif t == 0xA6 or t == 0xAC or t == 0xAD or t == 0xAE then
    r.service = t == 0xA6 and "getVariableAccessAttributes" or (t == 0xAC and "getNamedVariableListAttributes" or "deleteNamedVariableList")
    if req and s.kids[1] then
      if s.kids[1].tag == 0xA0 and s.kids[1].kids[1] then setname(r, s.kids[1].kids[1], false) else setname(r, s.kids[1], false) end
    end
  elseif t == 0x82 or t == 0xA2 then
    r.service = "identify"
  else
    r.service = string.format("service_%02x", t)
  end
end

local function interpret(kind, tag, kids)
  local r = { kind = kind, service = "", invoke_id = -1, scope = "", domain = "", target = "", names = {}, vtypes = {}, values = {}, quals = {} }
  if tag == 0xA0 or tag == 0xA1 then
    if #kids >= 2 then
      r.invoke_id = int(kids[1].d)
      confirmed(r, tag == 0xA0, kids[2])
    end
  elseif tag == 0xA2 then
    if kids[1] then r.invoke_id = int(kids[1].d) end
  elseif tag == 0xA3 then
    local ir = kids[1]
    if ir and ir.tag == 0xA0 and #ir.kids >= 2 then
      r.service = "informationReport"
      varspec(r, ir.kids[1])
      results(r, ir.kids[2])
    end
  end
  return r
end

-- ---- TASE.2 state and naming (same as scripts/main.zeek) --------------------------------------------------

local streams, pending, done = {}, {}, {} -- per tcp stream state; open requests; per-frame cache

local function bare(item) return item:match("^[^/]*/(.*)$") or item end

local function name_class(st, item)
  local n = bare(item)
  if special[n] then return n:lower() end
  if st.tsets[n] then return "transfer_set" end
  if st.datasets[item] then return "dataset" end
  if n:match("^IM_") or n:match("^Info") then return "infomsg" end
  return (st.tase2 and item:find("/", 1, true)) and "data" or ""
end

local function last_member(v)
  local last = ""
  for part in v:gsub("[{}]", ""):gmatch("[^;]+") do last = part end
  return last
end

local function conditions(b)
  local t = {}
  for i, name in ipairs(ds_condition_bits) do if b:sub(i, i) == "1" then t[#t + 1] = name end end
  return table.concat(t, ",")
end

local function mark(st, names, target)
  for _, n in ipairs(names) do if special[bare(n)] then st.tase2 = true end end
  if special[bare(target)] then st.tase2 = true end
end

local function learn(st, name, value)
  local n = bare(name)
  if n == "Bilateral_Table_ID" then st.bt = value
  elseif n == "TASE2_Version" then st.version = value
  elseif n == "Supported_Features" then st.features = value
  elseif n == "Next_DSTransfer_Set" then st.tsets[last_member(value)] = true end
end

local function item(st, name, vtype, value, qual, result, scope, domain)
  local class = name_class(st, name)
  if bit_classes[class] and vtype == "stateq" then vtype, qual = "bitstring", "" end
  local i = { name = name, class = class, type = vtype or "", value = value or "", quality = qual or "", result = result or "",
              scope = scope or "", domain = domain or "" }
  local d = name:match("^([^/]*)/")
  if d then i.scope, i.domain = "domain", d end
  return i
end

-- One frame's PDU -> { items, op, summary fields } or nil when the connection is not (yet) TASE.2.
local function process(stream, r)
  local st = streams[stream]
  if not st then st = { tsets = {}, datasets = {} }; streams[stream] = st end
  local out = { pdu = r.kind, service = r.service, invoke_id = r.invoke_id, items = {}, op = "" }
  local key = stream .. ":" .. r.invoke_id

  if r.kind == "confirmed_req" then
    mark(st, r.names, r.target)
    -- ponytail: an unanswered request stays in this table for the life of the capture; bound it if that ever matters.
    pending[key] = { service = r.service, names = r.names, scope = r.scope, domain = r.domain, target = r.target,
                     vtypes = r.vtypes, values = r.values, quals = r.quals }
    if r.service == "defineNamedVariableList" and st.tase2 then st.datasets[r.target] = true end
    out.op = r.service
    for i, n in ipairs(r.names) do
      out.items[#out.items + 1] = item(st, n, r.vtypes[i], r.values[i], r.quals[i], "", r.scope, r.domain)
    end
    out.dataset = r.service == "defineNamedVariableList" and r.target or nil
  elseif r.kind == "confirmed_resp" or r.kind == "confirmed_err" then
    local req = pending[key]
    pending[key] = nil
    if req then
      if req.service == "read" then
        out.op = "read"
        for i, v in ipairs(r.values) do
          local nm = req.names[i] or ""
          if r.kind == "confirmed_resp" and r.vtypes[i] ~= "error" then learn(st, nm, v) end  -- a refused read teaches nothing (its value is the error code)
          out.items[#out.items + 1] = item(st, nm, r.vtypes[i], v, r.quals[i], r.vtypes[i] == "error" and "failure" or "success", req.scope, req.domain)
        end
        if r.kind == "confirmed_err" then
          for _, nm in ipairs(req.names) do out.items[#out.items + 1] = item(st, nm, "", "", "", "error", req.scope, req.domain) end
        end
      elseif req.service == "write" then
        out.op = "write"
        for i, nm in ipairs(req.names) do
          local res = r.kind == "confirmed_err" and "error" or (r.values[i] or "")
          local it = item(st, nm, req.vtypes[i], req.values[i], req.quals[i], res, req.scope, req.domain)
          if it.class == "transfer_set" then out.op = "start_transfer_set" end
          out.items[#out.items + 1] = it
        end
      elseif req.service == "defineNamedVariableList" then
        out.op = "define_dataset"
        out.dataset = req.target
        out.items[1] = item(st, req.target, "", table.concat(req.names, ","), "", r.kind == "confirmed_err" and "error" or "success", req.scope, req.domain)
      else
        out.op = req.service
      end
    end
    if r.listing then -- getNameList response: the names themselves
      out.op = "getNameList"
      for _, n in ipairs(r.names) do out.items[#out.items + 1] = item(st, n, "", "", "", "", "", "") end
    end
  elseif r.kind == "unconfirmed" and r.service == "informationReport" then
    mark(st, r.names, r.target)
    local pos = r.names
    if #r.names == 0 and r.target ~= "" and #r.values >= 3 and r.vtypes[1] == "struct" and r.vtypes[3] == "bitstring" then
      pos = report_header
      st.tase2 = true
    end
    out.op = "report"
    out.dataset = r.target ~= "" and r.target or nil
    for i, v in ipairs(r.values) do
      local nm = pos[i] or (r.target ~= "" and string.format("%s[%d]", r.target, i - 1) or "")
      local b = bare(nm)
      if b == "Transfer_Set_Name" then out.tset = last_member(v)
      elseif b == "Transfer_Set_Time_Stamp" then out.timetag = v
      elseif b == "DSConditions_Detected" then out.conds = conditions(v) end
      out.items[#out.items + 1] = item(st, nm, r.vtypes[i], v, r.quals[i], "", r.scope, r.domain)
    end
    out.tset = out.tset or out.dataset
  elseif r.kind == "initiate_req" or r.kind == "initiate_resp" then
    out.op = "initiate"
  end

  if not st.tase2 then return nil end
  out.st = st
  return out
end

-- ---- post-dissector ---------------------------------------------------------------------------------------

local function dissect(tvb, pinfo, tree)
  local stream = f_stream()
  if not stream then return end
  for _, e in ipairs(pdu_fields) do
    local fi = e[4]()
    if fi then
      local s = fi.range:raw() -- the field's own range: after COTP reassembly it is not in the frame tvb
      local kids = parse(s, 1, #s, 1)
      if not kids then return end
      local r = interpret(e[2], e[3], kids)
      return process(tostring(stream), r)
    end
  end
end

function p.dissector(tvb, pinfo, tree)
  local n = pinfo.number
  local out = done[n]
  if out == nil then
    local ok, res = pcall(dissect, tvb, pinfo, tree)
    out = ok and res or false -- ponytail: a Lua error leaves the frame untagged; the MMS tree is unaffected
    done[n] = out
  end
  if not out then return end
  local t = tree:add(p, "ICCP / TASE.2")
  t:add(F.is_tase2, true):set_generated()
  t:add(F.pdu, out.pdu):set_generated()
  if out.service ~= "" then t:add(F.service, out.service):set_generated() end
  if out.invoke_id >= 0 then t:add(F.invoke_id, out.invoke_id):set_generated() end
  if out.op ~= "" then t:add(F.op, out.op):set_generated() end
  local st = out.st
  if st.bt then t:add(F.bt, st.bt):set_generated() end
  if st.version then t:add(F.version, st.version):set_generated() end
  if st.features then t:add(F.features, st.features):set_generated() end
  if out.dataset then t:add(F.dataset, out.dataset):set_generated() end
  if out.tset then t:add(F.tset, out.tset):set_generated() end
  if out.conds and out.conds ~= "" then t:add(F.conds, out.conds):set_generated() end
  if out.timetag then t:add(F.timetag, out.timetag):set_generated() end
  local info = {}
  for _, i in ipairs(out.items) do
    local it = t:add(p, i.name ~= "" and i.name or "(unnamed)")
    it:add(F.object, i.name):set_generated()
    if i.class ~= "" then it:add(F.class, i.class):set_generated() end
    if i.scope ~= "" then it:add(F.scope, i.scope):set_generated() end
    if i.domain ~= "" then it:add(F.domain, i.domain):set_generated() end
    if i.type ~= "" then it:add(F.type, i.type):set_generated() end
    if i.value ~= "" then it:add(F.value, i.value):set_generated() end
    if i.quality ~= "" then it:add(F.quality, i.quality):set_generated() end
    if i.result ~= "" then it:add(F.result, i.result):set_generated() end
    if #info < 3 then info[#info + 1] = i.name .. (i.value ~= "" and ("=" .. i.value) or "") end
  end
  local more = #out.items > 3 and (" +" .. (#out.items - 3)) or ""
  pinfo.cols.info:append(" | ICCP " .. out.op .. (out.dataset and (" " .. out.dataset) or "") .. " " .. table.concat(info, " ") .. more)
end

register_postdissector(p)
