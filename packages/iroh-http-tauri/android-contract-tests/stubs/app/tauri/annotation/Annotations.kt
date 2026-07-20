package app.tauri.annotation

@Target(AnnotationTarget.FUNCTION)
annotation class Command

@Target(AnnotationTarget.CLASS)
annotation class InvokeArg

@Target(AnnotationTarget.CLASS)
annotation class TauriPlugin
