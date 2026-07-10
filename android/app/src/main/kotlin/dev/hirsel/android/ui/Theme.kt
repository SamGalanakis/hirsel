package dev.hirsel.android.ui

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.sp

/**
 * hirsel's calm-terminal palette (DESIGN.md), authored in OKLCH and converted to
 * sRGB for Compose. Dark by commitment: near-black canvas, hairline white borders,
 * one muted-indigo accent, and a disciplined semantic status set. 16px is the top
 * of the type scale — confidence comes from spacing and restraint, not size.
 */
object Hirsel {
    // Neutrals
    val Background = Color(0xFF09090B) // oklch(0.141 0.005 285.823)
    val Card = Color(0xFF18181B) // oklch(0.21 0.006 285.885)
    val SurfaceRaised = Color(0xFF1F1F22)
    val Secondary = Color(0xFF27272A) // oklch(0.274 0.006 286.033)
    val Foreground = Color(0xFFFAFAFA) // oklch(0.985 0 0)
    val MutedForeground = Color(0xFFA1A1AA) // oklch(0.705 0.015 286.067)

    // Hairlines — white at low opacity does the structural work.
    val Border = Color(0x1AFFFFFF) // white / 10%
    val InputBorder = Color(0x26FFFFFF) // white / 15%

    // Brand accent — the single interaction / "attend to this" colour.
    val Accent = Color(0xFF4A6EBD) // oklch(0.55 0.13 264.05)
    val AccentRing = Color(0xFF5D82D6) // oklch(0.62 0.14 264.05)

    // Semantic status (sparingly).
    val StatusActive = Color(0xFF3BB5D7) // oklch(0.72 0.115 221)
    val StatusSuccess = Color(0xFF2AC362) // oklch(0.72 0.185 150)
    val StatusDanger = Color(0xFFFF6467) // oklch(0.704 0.191 22.216)
    val StatusAttention = Color(0xFFEEAE13) // oklch(0.79 0.16 82)
    val StatusIdle = Color(0xFF9A9A9A) // oklch(0.65 0 0)
}

// System sans (Roboto on Android) stands in for Inter's humanist register; machine
// tokens — tickets, codes, ids — earn the monospace stack.
private val Sans = FontFamily.Default
val HirselMono = FontFamily.Monospace

// Type scale capped at 16sp title (DESIGN.md "No-Display Rule").
private val HirselTypography = Typography(
    titleLarge = TextStyle(fontFamily = Sans, fontWeight = FontWeight.SemiBold, fontSize = 16.sp, lineHeight = 24.sp),
    titleMedium = TextStyle(fontFamily = Sans, fontWeight = FontWeight.SemiBold, fontSize = 15.sp, lineHeight = 22.sp),
    bodyLarge = TextStyle(fontFamily = Sans, fontWeight = FontWeight.Normal, fontSize = 14.sp, lineHeight = 22.sp),
    bodyMedium = TextStyle(fontFamily = Sans, fontWeight = FontWeight.Normal, fontSize = 14.sp, lineHeight = 21.sp),
    labelLarge = TextStyle(fontFamily = Sans, fontWeight = FontWeight.Medium, fontSize = 13.sp, lineHeight = 18.sp),
    labelMedium = TextStyle(fontFamily = Sans, fontWeight = FontWeight.Medium, fontSize = 12.sp, lineHeight = 16.sp),
)

private val HirselColorScheme = darkColorScheme(
    primary = Hirsel.Accent,
    onPrimary = Hirsel.Foreground,
    secondary = Hirsel.Secondary,
    onSecondary = Hirsel.Foreground,
    background = Hirsel.Background,
    onBackground = Hirsel.Foreground,
    surface = Hirsel.Card,
    onSurface = Hirsel.Foreground,
    surfaceVariant = Hirsel.Secondary,
    onSurfaceVariant = Hirsel.MutedForeground,
    outline = Hirsel.InputBorder,
    outlineVariant = Hirsel.Border,
    error = Hirsel.StatusDanger,
    onError = Hirsel.Foreground,
)

@Composable
fun HirselTheme(content: @Composable () -> Unit) {
    // Dark by commitment: the light theme exists only for token parity, never ships.
    @Suppress("UNUSED_EXPRESSION")
    isSystemInDarkTheme()
    MaterialTheme(
        colorScheme = HirselColorScheme,
        typography = HirselTypography,
        content = content,
    )
}
