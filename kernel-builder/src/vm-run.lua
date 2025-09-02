#!/usr/bin/env luajit

local ffi = require("ffi")

-- Define the libkrun C API
ffi.cdef [[
    // libkrun functions we need
    int32_t krun_create_ctx();
    int32_t krun_free_ctx(uint32_t ctx_id);
    int32_t krun_set_log_level(uint32_t level);
    int32_t krun_set_vm_config(uint32_t ctx_id, uint8_t num_vcpus, uint32_t ram_mib);
    int32_t krun_set_root(uint32_t ctx_id, const char *root_path);
    int32_t krun_add_disk(uint32_t ctx_id, const char *block_id, const char *disk_path, bool read_only);
    int32_t krun_add_virtiofs(uint32_t ctx_id, const char *c_tag, const char *c_path);
    int32_t krun_set_root_disk_remount(uint32_t ctx_id, const char *device, const char *fstype, const char *options);
    int32_t krun_set_workdir(uint32_t ctx_id, const char *workdir_path);
    int32_t krun_set_exec(uint32_t ctx_id, const char *exec_path, const char *const argv[], const char *const envp[]);
    int32_t krun_set_kernel(uint32_t ctx_id, const char *kernel_path, uint32_t kernel_format, const char *initramfs, const char *cmdline);
    int32_t krun_start_enter(uint32_t ctx_id);

    // Constants
    static const int KRUN_KERNEL_FORMAT_RAW = 0;
]]

-- Load libkrun
local libkrun = ffi.load("/usr/local/lib/libkrun.dylib")
-- local libkrun = ffi.load("krun")

-- Helper function to check for errors
local function check_error(result, operation)
    if result < 0 then
        error(string.format("%s failed with error code: %d", operation, result))
    end
    return result
end

-- VM configuration structure
-- config = {
--   kernel = { path = "/path/to/kernel", cmdline = "..." },
--   vm = { vcpus = 1, ram_mib = 512 },
--   root = { type = "path|disk", path = "/path/to/root", read_only = false },
--   data_disks = { { id = "data0", path = "/path/to/disk", read_only = true }, ... },
--   data_paths = { { tag = "shared", path = "/host/path" }, ... },
--   workdir = "/",
--   command = { path = "/bin/bash", args = {} },
--   env = {},
--   log_level = nil
-- }

-- Main VM launcher function
local function launch_vm(config)
    print("Creating libkrun context...")
    local ctx = check_error(libkrun.krun_create_ctx(), "krun_create_ctx")

    -- Set log level if specified
    if config.log_level then
        check_error(libkrun.krun_set_log_level(config.log_level), "krun_set_log_level")
    end

    -- Configure VM resources
    local vm = config.vm or {}
    local vcpus = vm.vcpus or 1
    local ram_mib = vm.ram_mib or 512
    print(string.format("Configuring VM (%d vCPU, %dMB RAM)...", vcpus, ram_mib))
    check_error(libkrun.krun_set_vm_config(ctx, vcpus, ram_mib), "krun_set_vm_config")

    -- Set root filesystem
    local root = config.root
    if not root or not root.path then
        error("Root path is required")
    end

    if root.type == "path" then
        print("Setting root path: " .. root.path)
        check_error(libkrun.krun_set_root(ctx, root.path), "krun_set_root")
    elseif root.type == "disk" then
        print("Setting root disk: " .. root.path)
        local read_only = root.read_only or false
        check_error(libkrun.krun_add_disk(ctx, "/dev/vda", root.path, read_only), "krun_add_disk")
        check_error(libkrun.krun_set_root_disk_remount(ctx, "/dev/vda", "auto", nil), "krun_set_root_disk_remount")
    else
        error("Root type must be 'path' or 'disk'")
    end

    -- Add data disks if specified
    if config.data_disks then
        for i, disk in ipairs(config.data_disks) do
            print(string.format("Adding data disk %s: %s", disk.id or ("data" .. i), disk.path))
            local read_only = disk.read_only or false
            check_error(libkrun.krun_add_disk(ctx, disk.id or ("data" .. i), disk.path, read_only), "krun_add_disk")
        end
    end

    -- Add virtiofs paths if specified
    if config.data_paths then
        for i, virtio_path in ipairs(config.data_paths) do
            local tag = virtio_path.tag or ("shared" .. i)
            print(string.format("Adding virtiofs mount %s: %s", tag, virtio_path.path))
            check_error(libkrun.krun_add_virtiofs(ctx, tag, virtio_path.path), "krun_add_virtiofs")
        end
    end

    -- Set working directory
    local workdir = config.workdir or "/"
    print("Setting working directory: " .. workdir)
    check_error(libkrun.krun_set_workdir(ctx, workdir), "krun_set_workdir")

    -- Prepare command arguments
    local command = config.command
    if not command or not command.path then
        error("Command path is required")
    end

    local args = command.args or {}
    local argc = #args + 1                           -- +1 for command path
    local argv = ffi.new("const char*[?]", argc + 1) -- +1 for NULL terminator
    argv[0] = command.path
    for i, arg in ipairs(args) do
        argv[i] = arg
        -- print(string.format("Command arg %d: `%s`", i, arg))
    end
    argv[argc] = nil -- NULL terminator

    -- Prepare environment
    local env = config.env or {}
    local envc = #env
    local envp = ffi.new("const char*[?]", envc + 1) -- +1 for NULL terminator
    for i, envvar in ipairs(env) do
        envp[i - 1] = envvar
    end
    envp[envc] = nil -- NULL terminator

    print("Setting executable: " .. command.path)
    check_error(libkrun.krun_set_exec(ctx, command.path, argv + 1, envp), "krun_set_exec")

    -- Set kernel
    local kernel = config.kernel
    -- if not kernel or not kernel.path then
    --     error("Kernel path is required")
    -- end

    local cmdline = kernel.cmdline
    if cmdline then
        cmdline = cmdline .. " KRUN_INIT=" .. command.path
    end

    if kernel and kernel.path then
        print("Setting kernel: " .. kernel.path)
        check_error(libkrun.krun_set_kernel(ctx, kernel.path, ffi.C.KRUN_KERNEL_FORMAT_RAW, nil, cmdline),
            "krun_set_kernel")
    end

    print("Starting VM...")
    print("===========================================")

    -- This will start the VM and enter it (blocking call)
    local result = libkrun.krun_start_enter(ctx)

    -- If we reach here, the VM has exited
    if result < 0 then
        print(string.format("VM exited with error code: %d", result))
    else
        print("VM exited successfully")
    end

    -- Clean up context
    libkrun.krun_free_ctx(ctx)

    return result
end

-- Helper function to load config from Lua file
local function load_config_file(config_path)
    local config_func, err = loadfile(config_path)
    if not config_func then
        error("Failed to load config file: " .. err)
    end

    local config = config_func()
    if type(config) ~= "table" then
        error("Config file must return a table")
    end

    return config
end

-- Helper function to set nested values in config using dot notation
local function set_config_value(config, path, value)
    local keys = {}
    for key in path:gmatch("[^%.]+") do
        -- Check if key contains array index notation like "args[0]"
        local base_key, index = key:match("^([^%[]+)%[(%d+)%]$")
        if base_key and index then
            table.insert(keys, { type = "array", key = base_key, index = tonumber(index + 1) })
        else
            table.insert(keys, { type = "simple", key = key })
        end
    end

    if #keys == 0 then
        error("Invalid config path: " .. path)
    end

    local current = config
    for i = 1, #keys - 1 do
        local key_info = keys[i]
        local key = key_info.key

        if current[key] == nil then
            if key_info.type == "array" then
                current[key] = {}
            else
                current[key] = {}
            end
        elseif type(current[key]) ~= "table" then
            error(string.format("Cannot set %s: %s is not a table", path, table.concat(keys, ".", 1, i)))
        end

        current = current[key]

        -- If this is an array access, navigate to the specific index
        if key_info.type == "array" then
            local index = key_info.index
            if current[index] == nil then
                current[index] = {}
            elseif type(current[index]) ~= "table" then
                error(string.format("Cannot set %s: array element at index %d is not a table", path, index))
            end
            current = current[index]
        end
    end

    local final_key_info = keys[#keys]
    local final_key = final_key_info.key

    -- Try to convert value to appropriate type
    local converted_value = value
    if value == "true" then
        converted_value = true
    elseif value == "false" then
        converted_value = false
    elseif value == "nil" then
        converted_value = nil
    elseif value:match("^%-?%d+$") then
        converted_value = tonumber(value)
    elseif value:match("^%-?%d*%.%d+$") then
        converted_value = tonumber(value)
    end

    if final_key_info.type == "array" then
        -- Setting array element
        if current[final_key] == nil then
            current[final_key] = {}
        end
        local index = final_key_info.index
        current[final_key][index] = converted_value
        print(string.format("Override: %s = %s", path, tostring(converted_value)))
    else
        -- Setting regular field
        current[final_key] = converted_value
        print(string.format("Override: %s = %s", path, tostring(converted_value)))
    end
end

local function is_array(t)
    local i = 0
    for _ in pairs(t) do
        i = i + 1
        if t[i] == nil then return false end
    end
    return true
end

-- Helper function to pretty print configuration
local function _print_config(config, indent)
    indent = indent or 0
    local prefix = string.rep("  ", indent)

    if is_array(config) then
        -- Print as array
        for _, value in ipairs(config) do
            if type(value) == "table" then
                print(prefix .. "{")
                _print_config(value, indent + 1)
                print(prefix .. "}")
            elseif type(value) == "string" then
                print(prefix .. '"' .. value .. '"')
            else
                print(prefix .. tostring(value))
            end
        end
    else
        for key, value in pairs(config) do
            if type(value) == "table" then
                print(prefix .. key .. " = {")
                _print_config(value, indent + 1)
                print(prefix .. "}")
            elseif type(value) == "string" then
                print(prefix .. key .. ' = "' .. value .. '"')
            else
                print(prefix .. key .. " = " .. tostring(value))
            end
        end
    end
end

local function print_config(config)
    print("{")
    _print_config(config, 1)
    print("}")
end

-- Parse command line arguments
local function main()
    local config_path = nil
    local overrides = {}
    local dry_run = false

    local i = 1
    while i <= #arg do
        if arg[i] == "--config" then
            if i + 1 > #arg then
                error("--config requires a value")
            end
            config_path = arg[i + 1]
            i = i + 2
        elseif arg[i] == "--set" then
            if i + 1 > #arg then
                error("--set requires a value")
            end
            local override = arg[i + 1]
            local eq_pos = override:find("=")
            if not eq_pos then
                error("--set value must be in format 'path.to.field=value'")
            end
            local path = override:sub(1, eq_pos - 1)
            local value = override:sub(eq_pos + 1)
            table.insert(overrides, { path = path, value = value })
            i = i + 2
        elseif arg[i] == "--dry-run" then
            dry_run = true
            i = i + 1
        else
            error("Unknown argument: " .. arg[i])
        end
    end

    if not config_path then
        print("Usage: " .. arg[0] .. " --config <config_file.lua> [--set path.to.field=value] ... [--dry-run]")
        print("Examples:")
        print("  " .. arg[0] .. " --config vm-config.lua")
        print("  " .. arg[0] .. " --config vm-config.lua --set vm.vcpus=4 --set vm.ram_mib=2048")
        print("  " .. arg[0] .. " --config vm-config.lua --set command.path=/bin/sh --set root.read_only=true")
        print("  " .. arg[0] .. " --config vm-config.lua --set 'command.args[0]=-c' --set 'command.args[1]=/script.sh'")
        print("  " .. arg[0] .. " --config vm-config.lua --dry-run")
        print("  " .. arg[0] .. " --config vm-config.lua --set vm.vcpus=8 --dry-run")
        os.exit(1)
    end

    print("Loading configuration from: " .. config_path)
    local config = load_config_file(config_path)

    -- Apply command-line overrides
    for _, override in ipairs(overrides) do
        set_config_value(config, override.path, override.value)
    end

    if dry_run then
        print("\n=== DRY RUN: Final Configuration ===")
        print_config(config)
        print("=====================================")
        os.exit(0)
    end

    print("Launching libkrun VM from config...")
    local result = launch_vm(config)
    os.exit(result == 0 and 0 or 1)
end

-- Run main function
main()
