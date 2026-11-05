plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}

android {
    namespace = "net.allod.wallet"
    compileSdk = 35

    defaultConfig {
        applicationId = "net.allod.wallet"
        minSdk = 26
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0-poc"
    }

    buildTypes {
        release {
            isMinifyEnabled = false
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions {
        jvmTarget = "17"
    }
    buildFeatures {
        compose = true
    }
    packaging {
        jniLibs {
            useLegacyPackaging = true
        }
    }
}

dependencies {
    // Nostr: keys, NIP-44, events and relays (rust-nostr bindings).
    implementation("org.rust-nostr:nostr-sdk:0.44.2") {
        exclude(group = "net.java.dev.jna", module = "jna")
    }
    // JNA with its Android native libraries.
    implementation("net.java.dev.jna:jna:5.17.0@aar")
    // UnifiedPush: endpoint from the distributor (e.g. ntfy), Web Push decryption.
    implementation("org.unifiedpush.android:connector:3.3.5")
    // QR scanning and generation.
    implementation("com.journeyapps:zxing-android-embedded:4.3.0")

    implementation("androidx.core:core-ktx:1.15.0")
    implementation("androidx.activity:activity-compose:1.9.3")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.10.2")
    implementation(platform("androidx.compose:compose-bom:2024.12.01"))
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.material3:material3")
}
