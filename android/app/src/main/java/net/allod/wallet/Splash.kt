package net.allod.wallet

import android.app.Activity
import android.os.Build
import android.util.TypedValue
import android.view.Gravity
import android.view.animation.DecelerateInterpolator
import android.widget.FrameLayout
import android.widget.TextView
import androidx.annotation.RequiresApi

/** Bottom of the A in the splash icon, as a fraction of the icon height (allod_splash_vector: y 785, scaled 0.59 about 512). */
private const val GLYPH_BOTTOM = (512 + (785 - 512) * 0.59f) / 1024f

/**
 * Finishes the system splash (Android 12+) with the "allod" wordmark.
 *
 * The app takes over the live splash view, so the logo stays exactly where the system drew it.
 * Once the icon animation has played, the wordmark fades in under the logo while rising 10 dp,
 * holds briefly, then the whole splash fades into the app.
 */
@RequiresApi(Build.VERSION_CODES.S)
fun Activity.finishSplashWithWordmark() {
    splashScreen.setOnExitAnimationListener { splash ->
        val icon = splash.iconView
        if (icon == null) {
            splash.remove()
            return@setOnExitAnimationListener
        }
        val dp = resources.displayMetrics.density
        val iconAt = IntArray(2).also(icon::getLocationInWindow)
        val splashAt = IntArray(2).also(splash::getLocationInWindow)

        val wordmark = TextView(this).apply {
            text = "allod"
            typeface = resources.getFont(R.font.allod_geometric)
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 53f)
            setTextColor(0xFFF4F2EB.toInt())
            includeFontPadding = false
            alpha = 0f
            translationY = 10 * dp
        }
        splash.addView(
            wordmark,
            FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.WRAP_CONTENT,
                FrameLayout.LayoutParams.WRAP_CONTENT,
                Gravity.CENTER_HORIZONTAL or Gravity.TOP,
            ).apply { topMargin = (iconAt[1] - splashAt[1] + icon.height * GLYPH_BOTTOM + 31 * dp).toInt() },
        )

        // The app can be ready before the icon animation ends: let it finish first.
        val iconDone = splash.iconAnimationStart?.let { it.toEpochMilli() + (splash.iconAnimationDuration?.toMillis() ?: 0L) }
        val wait = ((iconDone ?: 0L) - System.currentTimeMillis()).coerceAtLeast(0L)

        wordmark.animate()
            .alpha(1f)
            .translationY(0f)
            .setStartDelay(wait)
            .setDuration(350)
            .setInterpolator(DecelerateInterpolator())
            .withEndAction {
                splash.animate()
                    .alpha(0f)
                    .setStartDelay(300)
                    .setDuration(250)
                    .withEndAction { splash.remove() }
                    .start()
            }
            .start()
    }
}
