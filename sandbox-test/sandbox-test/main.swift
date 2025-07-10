//
//  main.swift
//  sandbox-test
//
//  Created by Jan Noha on 09.07.2025.
//

import Foundation

//let files: [String]? = try FileManager.default.contentsOfDirectory(atPath: "bin")
//print(files ?? [])

let args = RustVec<RustString>()
for arg in CommandLine.arguments {
    args.push(value: RustString(arg))
}

run(args)
