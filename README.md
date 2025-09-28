# Easy Send

Another thing I vibe-coded for my needs. VST3/[CLAP](https://github.com/free-audio/clap) plugin for transmitting audio.

This plugin was inspired by [Senderella](https://www.kvraudio.com/product/senderella_by_subminimal), a plugin developed by [Subliminal](https://web.archive.org/web/20070709063557/http://subminimal.org/modulr.php) a long while ago, that does not work on newer hosts.

It allows you to bypass DAW limitations and send your audio signal wherever you want, and do whatever you want with it. Some modular systems and hosts limit your capability for feedback loops. I needed to make this for my Patcher presets. This allows for feedback loops, or, gain reduction meters in one Control Surface window. (although you would have to change channels manually for every Patcher instance)

# Installation

You can download the lastest version in [releases](https://github.com/lathymeria/Easy_Send/releases).

Unpack the archive. If you want to install CLAP version, put easy_send.clap in:

C:\Program Files\Common Files\CLAP\Lath Audio

And, for VST3 version, put easy_send.vst3 folder in:

C:\Program Files\Common Files\VST3\Lath Audio

# Framework Used

I used a lovely framework called NIH-Plug:
https://github.com/robbert-vdh/nih-plug
