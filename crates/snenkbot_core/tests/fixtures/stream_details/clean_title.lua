local title = values.targetChannelTitle
assert(type(title) == 'string', 'Current channel title is required')

local function is_whitespace(codepoint)
    return (codepoint >= 0x09 and codepoint <= 0x0D)
        or codepoint == 0x20 or codepoint == 0x85 or codepoint == 0xA0
        or codepoint == 0x1680
        or (codepoint >= 0x2000 and codepoint <= 0x200A)
        or codepoint == 0x2028 or codepoint == 0x2029
        or codepoint == 0x202F or codepoint == 0x205F or codepoint == 0x3000
end

local codepoints = {}
for _, codepoint in utf8.codes(title) do
    codepoints[#codepoints + 1] = codepoint
end

local cleaned = {}
local index = 1
while index <= #codepoints do
    local codepoint = codepoints[index]
    if codepoint == 0x7C or codepoint == 0x1F409 or codepoint == 0x1FA90 then
        while #cleaned > 0 and is_whitespace(cleaned[#cleaned]) do
            cleaned[#cleaned] = nil
        end
        repeat
            index = index + 1
        until index > #codepoints or codepoints[index] == 0x0A
    else
        cleaned[#cleaned + 1] = codepoint
        index = index + 1
    end
end

local first, last = 1, #cleaned
while first <= last and is_whitespace(cleaned[first]) do first = first + 1 end
while last >= first and is_whitespace(cleaned[last]) do last = last - 1 end
local parts = {}
for position = first, last do
    parts[#parts + 1] = utf8.char(cleaned[position])
end
return { cleanTitle = table.concat(parts) }
