package dev.hirsel.android.ui

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.sp

/**
 * hirsel's calm-terminal palette (DESIGN.md), authored in OKLCH and converted to
 * sRGB for Compose. Two shipped schemes — dark is the brand's resting state,
 * light is a first-class, user-selectable peer sharing the SAME canonical tokens
 * as the web PWA. Near-black/near-white canvases (never pure), hairline borders,
 * one muted-indigo accent, a disciplined semantic status set, 16px type ceiling.
 */
data class HirselColors(
    val isLight: Boolean,
    val Background: Color,
    val Card: Color,
    val SurfaceRaised: Color,
    val Secondary: Color,
    val Foreground: Color,
    val MutedForeground: Color,
    val Border: Color,
    val InputBorder: Color,
    val Accent: Color,
    val AccentRing: Color,
    /** Text/content that sits on a filled [Accent] surface (near-white in both schemes). */
    val OnAccent: Color,
    val StatusActive: Color,
    val StatusSuccess: Color,
    val StatusDanger: Color,
    val StatusAttention: Color,
    val StatusIdle: Color,
)

// Dark by commitment: near-black canvas, hairline white borders, one indigo accent.
val DarkColors = HirselColors(
    isLight = false,
    Background = Color(0xFF09090B), // oklch(0.141 0.005 285.823)
    Card = Color(0xFF18181B), // oklch(0.21 0.006 285.885)
    SurfaceRaised = Color(0xFF1F1F22),
    Secondary = Color(0xFF27272A), // oklch(0.274 0.006 286.033)
    Foreground = Color(0xFFFAFAFA), // oklch(0.985 0 0)
    MutedForeground = Color(0xFFA1A1AA), // oklch(0.705 0.015 286.067)
    Border = Color(0x1AFFFFFF), // white / 10%
    InputBorder = Color(0x26FFFFFF), // white / 15%
    Accent = Color(0xFF4A6EBD), // oklch(0.55 0.13 264.05)
    AccentRing = Color(0xFF5D82D6), // oklch(0.62 0.14 264.05)
    OnAccent = Color(0xFFFAFAFA),
    StatusActive = Color(0xFF3BB5D7), // oklch(0.72 0.115 221)
    StatusSuccess = Color(0xFF2AC362), // oklch(0.72 0.185 150)
    StatusDanger = Color(0xFFFF6467), // oklch(0.704 0.191 22.216)
    StatusAttention = Color(0xFFEEAE13), // oklch(0.79 0.16 82)
    StatusIdle = Color(0xFF9A9A9A), // oklch(0.65 0 0)
)

// Canonical shared light palette (see shared-light-palette.md). Off-white canvas,
// white paper cards, black-ink hairlines, darkened indigo + status set — every
// text/UI pair verified >= WCAG-AA. status-success + status-attention are nudged
// a touch darker than the source doc to clear AA on the light canvas (reported).
val LightColors = HirselColors(
    isLight = true,
    Background = Color(0xFFFAFAFB), // oklch(0.985 0.002 286) — off-white, faint cool tint
    Card = Color(0xFFFFFFFF), // oklch(1 0 0) — white paper
    SurfaceRaised = Color(0xFFF5F5F6), // oklch(0.97 0.001 286)
    Secondary = Color(0xFFF3F3F5), // oklch(0.965 0.002 286) — quiet fill
    Foreground = Color(0xFF19191C), // oklch(0.215 0.006 286) — calm near-black
    MutedForeground = Color(0xFF64636E), // oklch(0.505 0.016 286) — AA on card + canvas
    Border = Color(0x1A161620), // dark ink @ 10% (mirrors dark white/10%)
    InputBorder = Color(0x24161620), // dark ink @ 14% (mirrors dark white/15%)
    Accent = Color(0xFF3E64B9), // oklch(0.52 0.14 264) — darkened indigo
    AccentRing = Color(0xFF476DC3), // oklch(0.55 0.14 264)
    OnAccent = Color(0xFFFAFAFA), // oklch(0.985 0 0) — white on indigo
    StatusActive = Color(0xFF007799), // oklch(0.52 0.12 221)
    StatusSuccess = Color(0xFF008228), // oklch(0.52 0.17 149) — nudged from 0.55 for AA
    StatusDanger = Color(0xFFD40C1A), // oklch(0.55 0.22 27)
    StatusAttention = Color(0xFFA86200), // oklch(0.56 0.15 73) — nudged from 0.60 for AA
    StatusIdle = Color(0xFF717171), // oklch(0.55 0 0)
)

/** User's theme choice; SYSTEM follows the OS via [isSystemInDarkTheme]. */
enum class ThemeMode { SYSTEM, LIGHT, DARK }

/** The active scheme, read by every surface via `LocalHirselColors.current`. */
val LocalHirselColors = staticCompositionLocalOf { DarkColors }

// System sans (Roboto on Android) stands in for Inter's humanist register; machine
// tokens — tickets, codes, ids — earn the monospace stack.
private val Sans = FontFamily.Default
val HirselMono = FontFamily.Monospace

// Type scale capped at 16sp title (DESIGN.md "No-Display Rule").
private val HirselTypography = Typography(
    titleLarge = TextStyle(fontFamily = Sans, fontWeight = FontWeight.SemiBold, fontSize = 16.sp, lineHeight = 24.sp, letterSpacing = 0.15.sp),
    titleMedium = TextStyle(fontFamily = Sans, fontWeight = FontWeight.SemiBold, fontSize = 15.sp, lineHeight = 22.sp, letterSpacing = 0.1.sp),
    bodyLarge = TextStyle(fontFamily = Sans, fontWeight = FontWeight.Normal, fontSize = 14.sp, lineHeight = 22.sp),
    bodyMedium = TextStyle(fontFamily = Sans, fontWeight = FontWeight.Normal, fontSize = 14.sp, lineHeight = 21.sp),
    labelLarge = TextStyle(fontFamily = Sans, fontWeight = FontWeight.Medium, fontSize = 13.sp, lineHeight = 18.sp),
    labelMedium = TextStyle(fontFamily = Sans, fontWeight = FontWeight.Medium, fontSize = 12.sp, lineHeight = 16.sp),
)

private fun materialSchemeFor(c: HirselColors) =
    if (c.isLight) {
        lightColorScheme(
            primary = c.Accent,
            onPrimary = c.OnAccent,
            secondary = c.Secondary,
            onSecondary = c.Foreground,
            background = c.Background,
            onBackground = c.Foreground,
            surface = c.Card,
            onSurface = c.Foreground,
            surfaceVariant = c.Secondary,
            onSurfaceVariant = c.MutedForeground,
            outline = c.InputBorder,
            outlineVariant = c.Border,
            error = c.StatusDanger,
            onError = c.OnAccent,
        )
    } else {
        darkColorScheme(
            primary = c.Accent,
            onPrimary = c.OnAccent,
            secondary = c.Secondary,
            onSecondary = c.Foreground,
            background = c.Background,
            onBackground = c.Foreground,
            surface = c.Card,
            onSurface = c.Foreground,
            surfaceVariant = c.Secondary,
            onSurfaceVariant = c.MutedForeground,
            outline = c.InputBorder,
            outlineVariant = c.Border,
            error = c.StatusDanger,
            onError = c.Foreground,
        )
    }

@Composable
fun HirselTheme(mode: ThemeMode = ThemeMode.SYSTEM, content: @Composable () -> Unit) {
    val dark = when (mode) {
        ThemeMode.SYSTEM -> isSystemInDarkTheme()
        ThemeMode.LIGHT -> false
        ThemeMode.DARK -> true
    }
    val colors = if (dark) DarkColors else LightColors
    CompositionLocalProvider(LocalHirselColors provides colors) {
        MaterialTheme(
            colorScheme = materialSchemeFor(colors),
            typography = HirselTypography,
            content = content,
        )
    }
}
