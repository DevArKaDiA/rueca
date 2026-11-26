plugins { kotlin("jvm") }

group = "com.acme"

dependencies {
    implementation("com.squareup.okio:okio:3.9.0")
    testImplementation(kotlin("test"))
}
