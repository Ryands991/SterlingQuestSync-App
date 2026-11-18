-- Sterling Quest Sync Companion add-on. Installed and kept up to date by the Sterling Quest Sync Companion app.
-- Inbox.lua (written by the app) sets SterlingQuestSync_Inbox. Sterling Quest Sync reads it
-- and writes outgoing copies into SterlingQuestSyncOut at logout. This file only makes sure both exist.

local f = CreateFrame("Frame")
f:RegisterEvent("ADDON_LOADED")
f:SetScript("OnEvent", function(_, _, name)
    if name ~= "SterlingQuestSync" then return end

    if type(SterlingQuestSyncOut) ~= "table" or SterlingQuestSyncOut.format ~= 1 then
        SterlingQuestSyncOut = { format = 1, characters = {} }
    end

    if type(SterlingQuestSync_Inbox) ~= "table" then
        SterlingQuestSync_Inbox = { format = 1, written = 0, chars = {} }
    end

    f:UnregisterEvent("ADDON_LOADED")
end)
