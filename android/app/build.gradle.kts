plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
    alias(libs.plugins.google.services)
}

val signingKeystorePath = providers.environmentVariable("SIGNING_KEYSTORE_PATH")
    .orElse(providers.gradleProperty("SIGNING_KEYSTORE_PATH"))
val signingKeystorePassword = providers.environmentVariable("SIGNING_KEYSTORE_PASSWORD")
    .orElse(providers.gradleProperty("SIGNING_KEYSTORE_PASSWORD"))
val signingKeyAlias = providers.environmentVariable("SIGNING_KEY_ALIAS")
    .orElse(providers.gradleProperty("SIGNING_KEY_ALIAS"))
val signingKeyPassword = providers.environmentVariable("SIGNING_KEY_PASSWORD")
    .orElse(providers.gradleProperty("SIGNING_KEY_PASSWORD"))
val hasReleaseSigning = listOf(
    signingKeystorePath,
    signingKeystorePassword,
    signingKeyAlias,
    signingKeyPassword,
).all { it.orNull?.isNotBlank() == true }

android {
    namespace = "dev.hirsel.android"
    compileSdk = 37

    defaultConfig {
        applicationId = "dev.hirsel.android"
        minSdk = 26
        targetSdk = 37
        versionCode = 1
        versionName = "0.1"

        ndk {
            abiFilters += listOf("arm64-v8a", "x86_64")
        }
    }

    signingConfigs {
        create("release") {
            if (hasReleaseSigning) {
                storeFile = file(signingKeystorePath.get())
                storePassword = signingKeystorePassword.get()
                keyAlias = signingKeyAlias.get()
                keyPassword = signingKeyPassword.get()
            }
        }
    }

    buildTypes {
        getByName("release") {
            signingConfig = if (hasReleaseSigning) {
                signingConfigs.getByName("release")
            } else {
                signingConfigs.getByName("debug")
            }
        }
    }

    buildFeatures {
        compose = true
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

dependencies {
    implementation(platform(libs.androidx.compose.bom))
    implementation(platform(libs.firebase.bom))
    implementation(libs.androidx.activity.compose)
    implementation(libs.androidx.compose.ui)
    implementation(libs.androidx.compose.ui.tooling.preview)
    implementation(libs.androidx.compose.material3)
    implementation(libs.firebase.messaging)
    implementation(libs.jna) {
        artifact {
            type = "aar"
        }
    }
    debugImplementation(libs.androidx.compose.ui.tooling)
}
