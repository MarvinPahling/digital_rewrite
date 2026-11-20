java := "/Library/Java/JavaVirtualMachines/temurin-27.jdk/Contents/Home/bin/java"
build_jar := "./references/Digital_release/Digital.jar"

default:
    just --list

run:
    {{ java }} -jar {{ build_jar }} 

build_reference:
    echo "TODO"

build_reimplemtation:
    echo "TODO"
