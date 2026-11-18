-- Feed age in seconds for the Basilisk Power widget.
--
-- Reads updated_unix directly from the same status.json the skin's WebParser
-- uses. Doing this in Lua keeps the proven WebParser regex untouched.
--
-- Returns:
--   >= 0  age of status.json, computed from its updated_unix field
--   -1    no file, no parsed timestamp (first load), or invalid data

function Update()
    local url = SKIN:GetVariable('JsonUrl')
    if not url or url == '' then
        return -1
    end

    -- file:///C:/Users/... -> C:\Users\...
    local path = url:gsub('^file:///', ''):gsub('/', '\\')

    local file = io.open(path, 'r')
    if not file then
        return -1
    end
    local text = file:read('*a')
    file:close()

    local unix = text and text:match('"updated_unix"%s*:%s*(%d+)')
    unix = tonumber(unix)
    if not unix or unix <= 0 then
        return -1
    end

    local age = os.time() - unix
    if age < 0 then
        -- A clock adjustment can put the feed slightly in the future; treat
        -- that as fresh rather than as a negative (unknown) age.
        return 0
    end
    return age
end
