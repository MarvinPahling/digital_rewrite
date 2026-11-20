java := "/Library/Java/JavaVirtualMachines/temurin-27.jdk/Contents/Home/bin/java"
build_jar := "./references/Digital_release/Digital.jar"

run:
    {{ java }} -jar {{ build_jar }} 
