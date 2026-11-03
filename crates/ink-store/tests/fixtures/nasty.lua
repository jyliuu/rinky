-- nasty.lua
return {
    ["cjk"] = "中文 text",
    ["empty_table"] = {},
    ["emoji"] = "😀",
    ["bad_utf8"] = "x�y",
    ["nested_list"] = {
        [1] = {
            [1] = 1,
            [2] = 2,
        },
        [2] = {
            [1] = "q",
        },
    },
    ["empty"] = "",
    ["int"] = 42,
    ["neg"] = -7,
    ["zero"] = 0,
    ["half"] = 0.5,
    ["third"] = 0.33333333333333,
    ["big"] = 1e+300,
    ["tiny"] = 1e-300,
    ["two53"] = 9.007199254741e+15,
    ["negzero"] = 0,
    ["neghuge"] = -inf,
    ["quote"] = "say \"hi\"",
    ["no"] = false,
    ["backslash"] = "a\\b\\\\c",
    ["yes"] = true,
    ["newline"] = "line1\
line2\
",
    ["boolkeys"] = {
        [false] = "f",
        [true] = "t",
    },
    ["cr"] = "a\13b",
    ["numkeys"] = {
        [0] = "z",
        [2.5] = "f",
        [-1] = "m",
    },
    ["nul"] = "a\0b",
    ["holes"] = {
        [3] = "c",
        [1] = "a",
    },
    ["nul_digit"] = "a\0001b",
    ["list"] = {
        [1] = 10,
        [2] = 20,
        [3] = 30,
        [4] = "x",
        [5] = {
            [1] = 1,
            [2] = 2,
        },
    },
    ["nested"] = {
        ["a"] = {
            ["b"] = {
                ["c"] = {
                    ["d"] = "deep",
                },
            },
        },
    },
    ["controls"] = "\1\2\7\8\9\11\12\27\31\127",
    ["huge"] = inf,
}
