local arguments = values.arguments
assert(type(arguments) == 'string', 'Command arguments are required')
return { input0 = arguments:match('^[^ ]+') or '' }
