-- Axel's actions for Rex. `axel rex setup` writes this file and keeps it
-- current: edits here are overwritten. Remove it with `axel rex setup --remove`.
--
-- axel_ask puts a question to the owner in every connected Rex app, the iOS
-- app included; the first answer wins. Axel calls it with client.action and
-- reads the outcome. Only a configuration-file action can ask (rex.ui), which
-- is why this lives here and not in a script Axel runs.
--
-- The other actions are Axel's commands in Rex's command palette, under
-- "Axel". Some run the axel CLI; the ones about an open thread ask that
-- thread's composer, a Rex client labelled axel:<window>, with client.perform.

local MODULE_VERSION = 2
-- Filled in by `axel rex setup`: the axel that installed this, and the PATH
-- it ran with, so the commands Rex runs find the agents' CLIs.
local AXEL = [==[@AXEL@]==]
local PATH = [==[@PATH@]==]
local COMPOSER_PREFIX = "axel:"

local function outcome_of(answer, why, detail)
  if answer == nil then
    return { v = MODULE_VERSION, outcome = why, detail = detail }
  end
  if type(answer) == "table" and answer.text ~= nil then
    return { v = MODULE_VERSION, outcome = "text", text = answer.text }
  end
  if type(answer) == "string" then
    answer = { answer }
  end
  return { v = MODULE_VERSION, outcome = "ids", ids = answer, count = #answer }
end

rex.action {
  name = "axel_ask",
  title = "Axel: Ask the owner",
  category = "Axel",
  run = function(ctx, args)
    if args == nil or type(args.title) ~= "string" then
      error("axel_ask: title is required")
    end
    local spec = {
      title = args.title,
      placeholder = args.placeholder,
      confirm = args.confirm,
      timeout = args.timeout,
    }
    if args.items == nil then
      local text, why, detail = rex.ui.input(spec)
      if text == nil then
        return outcome_of(nil, why, detail)
      end
      return outcome_of({ text = text })
    end
    spec.items = args.items
    spec.multiple = args.multiple
    spec.allow_other = args.allow_other
    return outcome_of(rex.ui.pick(spec))
  end,
}

-- The environment an axel command runs with: the session and block the
-- palette was opened over, so new windows open there.
local function env_for(ctx)
  return { PATH = PATH, REX_SESSION = ctx.session_id, REX_BLOCK = ctx.block_id }
end

local function argv_of(args)
  local argv = { AXEL }
  for _, a in ipairs(args) do
    argv[#argv + 1] = a
  end
  return argv
end

-- Run axel and decode what it prints as JSON; raise its error otherwise.
local function axel_json(ctx, args)
  local r, err = rex.exec(argv_of(args), { env = env_for(ctx), timeout = 20 })
  if r == nil then
    error("axel: " .. err)
  end
  if not r.ok then
    error("axel: " .. (r.stderr ~= "" and r.stderr or ("exit " .. tostring(r.code))))
  end
  local value, json_err = rex.json.decode(r.stdout)
  if value == nil then
    error("axel: " .. json_err)
  end
  return value
end

-- Start axel and let it run on after this action ends (it opens windows,
-- or waits for an answer).
local function axel_start(ctx, args)
  local handle, err = rex.exec.start(argv_of(args), { env = env_for(ctx), detach = true, stdout = false, stderr = false })
  if handle == nil then
    error("axel: " .. err)
  end
end

rex.action {
  name = "axel_answer",
  title = "Axel: Answer a waiting question",
  category = "Axel",
  keywords = "question agent reply blocked",
  run = function(ctx)
    local questions = axel_json(ctx, { "thread", "questions", "--json" })
    if #questions == 0 then
      error("No agent is waiting for an answer")
    end
    local chosen = questions[1]
    if #questions > 1 then
      local items = {}
      for i, q in ipairs(questions) do
        items[#items + 1] = { id = tostring(i), title = q.prompt, subtitle = "@" .. q.agent .. " · " .. q.thread_title, icon = "person" }
      end
      local id = rex.ui.pick { title = "Answer a waiting question", placeholder = "Filter questions", items = items }
      if id == nil then
        return
      end
      chosen = questions[tonumber(id)]
    end
    axel_start(ctx, { "thread", "ask", chosen.thread_id, chosen.item_id })
  end,
}

rex.action {
  name = "axel_open_thread",
  title = "Axel: Open a thread",
  category = "Axel",
  keywords = "thread conversation switch",
  run = function(ctx)
    local threads = axel_json(ctx, { "thread", "ls", "--json" })
    local items = {}
    for _, entry in ipairs(threads) do
      local t = entry.thread
      if t.state == "open" then
        items[#items + 1] = { id = t.id, title = t.title, icon = "terminal" }
      end
    end
    if #items == 0 then
      error("No open thread")
    end
    local id = rex.ui.pick { title = "Open a thread", placeholder = "Filter threads", items = items }
    if id == nil then
      return
    end
    axel_start(ctx, { "thread", "split", id })
  end,
}

rex.action {
  name = "axel_new_thread",
  title = "Axel: New thread",
  category = "Axel",
  keywords = "thread agent start task prompt",
  run = function(ctx)
    local agents = axel_json(ctx, { "agent", "ls", "--json" })
    if #agents == 0 then
      error("No agent yet: run axel setup")
    end
    local name = agents[1].name
    if #agents > 1 then
      local items = {}
      for _, a in ipairs(agents) do
        items[#items + 1] = { id = a.name, title = "@" .. a.name, subtitle = a.harness, icon = "person" }
      end
      name = rex.ui.pick { title = "New thread with", placeholder = "Filter agents", items = items }
      if name == nil then
        return
      end
    end
    local text = rex.ui.input { title = "What should @" .. name .. " do?", confirm = "Start" }
    if text == nil or text:match("^%s*$") then
      return
    end
    -- In the directory of the terminal the palette opened over, if it has one.
    local args = { "thread", "new", "--agent", name }
    local here = ctx.block_id and rex.block.call("com.superlogical.terminal", "pwd", { block_id = ctx.block_id })
    if here and here.pwd and here.pwd ~= "" then
      args[#args + 1] = "--project"
      args[#args + 1] = here.pwd
    else
      args[#args + 1] = "--scratch"
    end
    args[#args + 1] = "--"
    args[#args + 1] = text
    axel_start(ctx, args)
  end,
}

-- The composer of the thread in front of you: the one in the palette's
-- window, else the only one in this session, else the one you pick.
local function composer(ctx)
  local listed, err = rex.call("client.list", {})
  if listed == nil then
    error("axel: " .. err)
  end
  local window = nil
  if ctx.block_id then
    local described = rex.session.describe_block { block_id = ctx.block_id }
    window = described and described.block and described.block.window_id
  end
  local here = {}
  for _, c in ipairs(listed.clients or {}) do
    local label = c.info and c.info.label or ""
    if label:sub(1, #COMPOSER_PREFIX) == COMPOSER_PREFIX then
      if window and label == COMPOSER_PREFIX .. window then
        return c.client_id
      end
      for _, s in ipairs(c.session_ids or {}) do
        if s == ctx.session_id then
          here[#here + 1] = c.client_id
        end
      end
    end
  end
  if #here == 0 then
    error("No Axel thread is open here")
  end
  if #here == 1 then
    return here[1]
  end
  local items = {}
  for _, id in ipairs(here) do
    local described = rex.call("client.perform", { client_id = id, action = "axel.describe" })
    local title = described and described.result and described.result.title or id
    items[#items + 1] = { id = id, title = title, icon = "terminal" }
  end
  return rex.ui.pick { title = "Which thread?", items = items }
end

-- An action that asks the thread's composer to perform `action`.
local function on_thread(name, title, action, keywords)
  rex.action {
    name = name,
    title = title,
    category = "Axel",
    keywords = keywords,
    run = function(ctx)
      local client = composer(ctx)
      if client == nil then
        return
      end
      local done, err = rex.call("client.perform", { client_id = client, action = action, origin = "palette", session_id = ctx.session_id })
      if done == nil then
        error(err)
      end
      return done.result
    end,
  }
end

on_thread("axel_stop", "Axel: Stop the agent's turn", "axel.stop", "cancel interrupt halt")
on_thread("axel_thread_controls", "Axel: Thread controls", "axel.controls", "sessions spend archive settle")
on_thread("axel_agent_log", "Axel: Agent log", "axel.agent_log", "session pane raw harness")
on_thread("axel_focus", "Axel: Focus the composer", "axel.focus", "prompt type write")

return { version = MODULE_VERSION }
