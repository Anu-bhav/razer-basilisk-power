[Variables]
Language=English
Location=Poste de Flacq
Unit=m
Hidden=0

; Clean, modern font selections that render '0' beautifully:
FontFace="MesloLGS NF Regular"
; FontFace="Segoe UI"  ; (Windows native fallback)
; FontFace="Ubuntu"    ; (Excellent rounded alternative)

FontColor1=0,202,255,100
FontColor2=255,43,255,100
FontColor3=255,255,255,100

FontColor1_Area=0,202,255,51
FontColor2_Area=255,43,255,51

; --- Neon variants -------------------------------------------------------
; The theme colours above carry alpha 100 (39% opacity), which is right for
; text but reads dull for a solid fill. These are the same hues at full
; saturation and value:
;
;   FontColor1 hue 192 -> #00CAFF  (theme cyan, full opacity)
;   FontColor2 hue 300 -> #FF2BFF  (theme magenta, full opacity)
;   NeonBlue is the hue of the Simplic CPU/RAM line graph (205 deg) at full
;   saturation and value -- the vivid azure that graph draws.
;
; Used for the battery fill so it reads as vividly as the CPU/RAM widgets.
FontColor1_Neon=0,202,255,255
FontColor2_Neon=255,43,255,255
NeonBlue=0,149,255,255

; Low-battery warning: a vivid coral red that still reads as part of the
; neon set. Used only when the level is at or below #LowBatteryPercent# and
; the mouse is off the dock.
LowBattery=255,64,64,255


; --- ISP Speed Limits (Bits per second) ---
; 100 Mbps = 100,000,000 bits
MaxDownload=100000000
; 20 Mbps = 20,000,000 bits
MaxUpload=20000000